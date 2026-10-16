#!/usr/bin/env node
// `right-release publish npm|cargo ... --local`: publish ONE @rightkit npm package or crate from the Mac under
// Adrian's local-build grant (2026-10-14, committed .rightkit-local-build.json). It does not weaken the
// publication gate: it runs the unit's own CI tests (ci/gate.config.json, ci/asset-gated.json,
// scripts/ci/gate.mjs semantics), packs the artifact, writes a schema-1 evidence file with one darwin receipt
// (hostPolicy "darwin-only"), then calls the EXISTING enforcePublicationGate and the EXISTING publisher
// (publishGatedTarball for npm, runCargoPublish for cargo). There is no way to skip a test or the evidence.
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { spawnSync } from "./background-process.mjs";
import { runCargoSync } from "./managed-cargo.mjs";
import { localBuildGranted, LOCAL_BUILD_FILE } from "./execution-policy.mjs";
import { GATE_SCHEMA, defaultEvidencePath, enforcePublicationGate, publishGatedTarball } from "./publication-gate.mjs";
import { runCargoPublish } from "./publish-cargo.mjs";
import { cargoIndexUrl } from "./cargo-registry-readback.mjs";
import { readCargoPackage } from "./crate-graph.mjs";
import { recordPublished } from "./rightkit-sync.mjs";

export const LOCAL_OPERATOR = "agent (local-build grant)";
export const LOCAL_HOST_POLICY_REASON = "Adrian 2026-10-14 local-build grant (.rightkit-local-build.json); Windows covered by the next CI run";
const STEP_TIMEOUT_MS = 30 * 60 * 1000;

export class LocalPublishError extends Error {
  constructor(message) { super(message); this.name = "LocalPublishError"; }
}
const refuse = (message) => { throw new LocalPublishError(message); };

export function parseLocalPublishArgs(kind, argv, cwd = process.cwd()) {
  const flag = kind === "npm" ? "--package" : "--crate";
  let unit; let workspace; let local = false; let allowIgnored = false;
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === "--local") local = true;
    else if (arg === flag) unit = argv[++i];
    else if (arg === "--workspace") workspace = argv[++i];
    // Same meaning as publish.yml's allow_ignored: this unit may have ignored tests; the count is recorded.
    else if (arg === "--allow-ignored") allowIgnored = true;
    else refuse(`unknown publish ${kind} --local option: ${arg} (accepted: ${flag} <name>, --workspace <dir>, --allow-ignored, --local)`);
  }
  if (!local) refuse("--local is required");
  if (kind === "npm" && !/^@rightkit\/[a-z0-9-]+$/.test(unit ?? "")) refuse("publish npm --local requires --package @rightkit/<name>");
  if (kind === "cargo" && !/^[a-z0-9]+(?:-[a-z0-9]+)*$/.test(unit ?? "")) refuse("publish cargo --local requires --crate <name>");
  return { kind, unit, allowIgnored, workspaceRoot: path.resolve(workspace ?? cwd) };
}

function defaultGit(args, cwd) {
  const result = spawnSync("git", args, { cwd, encoding: "utf8", windowsHide: true });
  if (result.error || result.status !== 0) return null;
  return (result.stdout ?? "").trim();
}

const defaultReadJson = (file) => { try { return JSON.parse(fs.readFileSync(file, "utf8")); } catch { return null; } };

async function defaultRegistryHas({ kind, name, version }) {
  if (kind === "npm") {
    const view = spawnSync("npm", ["view", `${name}@${version}`, "version", "--prefer-online", "--registry", "https://registry.npmjs.org/"], { encoding: "utf8", windowsHide: true });
    const out = `${view.stdout ?? ""}${view.stderr ?? ""}`;
    if (!view.error && view.status === 0) return (view.stdout ?? "").trim() === version;
    if (/E404|404 Not Found/i.test(out)) return false;
    throw new Error(`cannot read the npm registry for ${name}@${version}: ${out.trim().slice(0, 200)}`);
  }
  const response = await fetch(cargoIndexUrl(name), { headers: { Accept: "text/plain", "Cache-Control": "no-cache", "User-Agent": "rightkit-release" }, redirect: "error", signal: AbortSignal.timeout(30_000) });
  if (response.status === 404) return false;
  if (!response.ok) throw new Error(`crates.io index answered ${response.status} for ${name}`);
  return (await response.text()).split(/\r?\n/).filter((line) => line.trim()).some((line) => { try { return JSON.parse(line).vers === version; } catch { return false; } });
}

function defaultRun(command, args, options) {
  const settings = { encoding: "utf8", maxBuffer: 512 * 1024 * 1024, timeout: STEP_TIMEOUT_MS, windowsHide: true, ...options };
  return command === "cargo" ? runCargoSync(args, settings) : spawnSync(command, args, settings);
}

async function defaultLoadWorkspaceModules(workspaceRoot) {
  const load = (file) => import(pathToFileURL(path.join(workspaceRoot, "scripts", "ci", file)).href);
  const [results, upload] = await Promise.all([load("test-results.mjs"), load("upload.mjs")]);
  return { results, checkNpmTarball: upload.checkNpmTarball };
}

/**
 * Feature sets that must at least compile, beyond the tested ones. A crate whose tested sets already
 * include `--all-features` needs none. `checkFeatureSets` (ci/gate.config.json) lists them explicitly for
 * crates with mutually exclusive features; otherwise one `--all-features` check runs. Mirrored in scripts/ci/gate.mjs.
 */
export function allFeatureCheckSets(entry = {}) {
  const tested = entry.featureSets ?? [[]];
  if (tested.some((features) => features.includes("--all-features"))) return [];
  return entry.checkFeatureSets ?? [["--all-features"]];
}

/** The CI gate's commands for one unit (scripts/ci/gate.mjs npmGate/rustGate), mirrored exactly. */
export function localTestPlan({ kind, unit, dir, config, manifest }) {
  if (kind === "npm") {
    if (!manifest?.scripts?.test) refuse(`${unit} has no test script; --local never publishes an untested package`);
    return {
      id: `npm:${dir}`, featureLabels: ["default"],
      steps: [
        { command: "pnpm", args: ["-r", "--workspace-concurrency=1", "run", "build"], test: false },
        { command: "pnpm", args: ["--dir", dir, "run", "test"], test: true },
      ],
    };
  }
  const entry = config?.rust?.[unit] ?? {};
  if (entry.os && !entry.os.includes("macos")) refuse(`${unit} is restricted to ${entry.os.join(", ")} by ci/gate.config.json; it cannot be qualified on this Mac`);
  const featureSets = entry.featureSets ?? [[]];
  const steps = [];
  for (const features of featureSets) {
    steps.push({ command: "cargo", args: ["test", "-p", unit, ...features, "--no-fail-fast", "--", "--show-output"], test: true });
    if (entry.alsoSerial) steps.push({ command: "cargo", args: ["test", "-p", unit, ...features, "--no-fail-fast", "--", "--test-threads=1", "--show-output"], test: true });
  }
  steps.push({ command: "cargo", args: ["clippy", "-p", unit, "--no-deps", "--all-targets", ...featureSets[0], "--", "-D", "warnings"], test: false });
  for (const features of allFeatureCheckSets(entry)) steps.push({ command: "cargo", args: ["check", "-p", unit, "--all-targets", ...features], test: false });
  return { id: `rust:${unit}`, featureLabels: featureSets.map((features) => features.join(" ") || "default"), steps };
}

const sha256 = (bytes) => crypto.createHash("sha256").update(bytes).digest("hex");
const tarballName = (name, version) => `${name.replace(/^@/, "").replace("/", "-")}-${version}.tgz`;
const tail = (text, lines = 40) => String(text ?? "").split(/\r?\n/).slice(-lines).join("\n");

export async function runLocalPublish(kind, argv, deps = {}) {
  const log = deps.log ?? ((line) => process.stdout.write(`${line}\n`));
  const { unit, allowIgnored, workspaceRoot } = parseLocalPublishArgs(kind, argv, deps.cwd);
  const platform = deps.platform ?? process.platform;
  const git = deps.git ?? defaultGit;
  const readJson = deps.readJson ?? defaultReadJson;
  const run = deps.run ?? defaultRun;

  // 1. Preconditions, fail closed.
  if (!(deps.localBuildGranted ?? localBuildGranted)(workspaceRoot)) refuse(`--local needs the local-build grant: a committed ${LOCAL_BUILD_FILE} (allow "build-test-publish") naming this repository at ${workspaceRoot}`);
  if (platform !== "darwin") refuse(`--local publishes from the Mac only (host policy: darwin-only); this host is ${platform}`);
  const dir = kind === "npm" ? `packages/${unit.slice("@rightkit/".length)}` : `crates/${unit}`;
  const unitPath = path.join(workspaceRoot, dir);
  if (!fs.existsSync(unitPath)) refuse(`unit directory not found: ${unitPath}`);
  let manifest = null; let version;
  if (kind === "npm") {
    manifest = readJson(path.join(unitPath, "package.json")) ?? refuse(`cannot read ${dir}/package.json`);
    if (manifest.name !== unit) refuse(`${dir}/package.json is ${manifest.name}, not ${unit}`);
    if (manifest.private === true) refuse(`${unit} is private and is never published`);
    version = manifest.version;
  } else {
    const pkg = (deps.readCargoPackage ?? readCargoPackage)(unitPath);
    if (pkg.name !== unit) refuse(`Cargo.toml [package].name ${JSON.stringify(pkg.name)} differs from --crate ${unit}`);
    version = pkg.version;
  }
  if (typeof version !== "string" || !version.trim()) refuse(`${unit} has no literal version`);
  const head = git(["rev-parse", "HEAD"], workspaceRoot);
  if (!head || !/^[0-9a-f]{40}$/.test(head)) refuse("cannot resolve HEAD");
  const dirty = git(["status", "--porcelain", "--", dir], workspaceRoot);
  if (dirty === null) refuse(`cannot read git status for ${dir}`);
  if (dirty !== "") refuse(`tree is not clean for ${dir}; commit first:\n${dirty}`);
  if (await (deps.registryHas ?? defaultRegistryHas)({ kind, name: unit, version })) refuse(`${unit}@${version} is already on the ${kind === "npm" ? "npm" : "crates.io"} registry; bump the version`);
  log(`[right-release] local publish ${kind}:${unit}@${version} at ${head.slice(0, 12)} (local-build grant, darwin-only policy)`);

  // 2. The unit's own tests, exactly as the CI gate defines them.
  const gateConfig = readJson(path.join(workspaceRoot, "ci", "gate.config.json")) ?? {};
  const assetGated = readJson(path.join(workspaceRoot, "ci", "asset-gated.json")) ?? {};
  const gatedSection = assetGated[kind === "npm" ? "npm" : "crates"] ?? {};
  const isAssetGated = Object.hasOwn(gatedSection, unit);
  const plan = localTestPlan({ kind, unit, dir, config: gateConfig, manifest });
  const modules = await (deps.loadWorkspaceModules ?? defaultLoadWorkspaceModules)(workspaceRoot);
  const { ResultScanner, TEST_COMMAND, judgeUnit } = modules.results;
  // Run the unit's tests under hosted-CI conditions: no inherited credentials (a managed host refuses a
  // token from the environment) and no sibling product checkouts (CI holds rightkit alone).
  const inherited = Object.fromEntries(Object.entries(deps.env ?? process.env).filter(([key]) => !/(TOKEN|SECRET|PASSWORD|API_KEY|PRIVATE_KEY)/i.test(key)));
  const env = { ...inherited, RIGHTKIT_REQUIRE_ASSETS: isAssetGated ? "0" : "1", RIGHTKIT_ISOLATED_CHECKOUT: "1", CARGO_TERM_COLOR: "never" };
  const steps = [];
  for (const step of plan.steps) {
    const commandText = `${step.command} ${step.args.join(" ")}`;
    log(`[right-release] ${plan.id}: ${commandText}`);
    const result = run(step.command, step.args, { cwd: workspaceRoot, env });
    const row = { command: commandText, ok: false, isTest: step.test && TEST_COMMAND.test(commandText) };
    if (result.error || result.status !== 0) {
      steps.push(row);
      refuse(`${plan.id} step failed (${result.error?.message ?? `exit ${result.status}`}): ${commandText}\n${tail(`${result.stdout ?? ""}\n${result.stderr ?? ""}`)}`);
    }
    row.ok = true;
    if (row.isTest) {
      const out = new ResultScanner(); const err = new ResultScanner();
      out.push(result.stdout ?? ""); err.push(result.stderr ?? "");
      const a = out.finish(); const b = err.finish();
      Object.assign(row, { executed: a.executed + b.executed, failed: a.failed + b.failed, ignored: a.ignored + b.ignored, skipped: a.skipped + b.skipped, binaries: a.binaries + b.binaries });
    }
    steps.push(row);
  }
  const verdict = judgeUnit({ name: unit, steps: steps.map((s) => ({ ...s, unit: plan.id })), allowIgnored: allowIgnored ? [unit] : [], allowMissingAssets: isAssetGated ? [unit] : [] });
  if (verdict.status === "fail" || steps.some((s) => s.failed > 0)) refuse(`${plan.id} not qualified: ${verdict.problems.join("; ") || "failed tests reported"}`);
  if (!(verdict.executed > 0)) refuse(`${plan.id} executed no test`);
  log(`[right-release] ${plan.id}: ${verdict.executed} passed, 0 failed, ${verdict.skipped} skipped, ${verdict.ignored} ignored${isAssetGated ? " (asset-gated: RIGHTKIT_REQUIRE_ASSETS=0, skips recorded)" : ""}`);

  // 3. Pack the artifact and hash it.
  const scratch = fs.mkdtempSync(path.join(deps.tmpdir ?? os.tmpdir(), "rk-local-publish-"));
  const cleanup = () => fs.rmSync(scratch, { recursive: true, force: true });
  try {
    let artifactFile; const packCommands = [];
    if (kind === "npm") {
      const args = ["--dir", dir, "pack", "--pack-destination", scratch];
      packCommands.push(`pnpm ${args.join(" ")}`);
      const packed = run("pnpm", args, { cwd: workspaceRoot, env });
      if (packed.error || packed.status !== 0) refuse(`pnpm pack failed for ${unit}\n${tail(`${packed.stdout ?? ""}\n${packed.stderr ?? ""}`)}`);
      artifactFile = path.join(scratch, tarballName(unit, version));
      if (!fs.existsSync(artifactFile)) refuse(`pnpm pack did not produce ${path.basename(artifactFile)}`);
      const checked = modules.checkNpmTarball({ file: artifactFile, name: unit, version, dir, spawn: deps.spawnSync ?? spawnSync });
      if (checked.problems.length) refuse(`${unit}: packed tarball refused:\n  - ${checked.problems.join("\n  - ")}`);
    } else {
      packCommands.push(`cargo package --locked -p ${unit}`);
      const packed = run("cargo", ["package", "--locked", "-p", unit], { cwd: workspaceRoot, env });
      if (packed.error || packed.status !== 0) refuse(`cargo package failed for ${unit}\n${tail(`${packed.stdout ?? ""}\n${packed.stderr ?? ""}`)}`);
      packCommands.push("cargo metadata --format-version 1 --no-deps");
      const meta = run("cargo", ["metadata", "--format-version", "1", "--no-deps"], { cwd: workspaceRoot, env });
      let targetDir;
      try { targetDir = JSON.parse(meta.stdout).target_directory; } catch { /* refused below */ }
      if (meta.status !== 0 || typeof targetDir !== "string" || !path.isAbsolute(targetDir)) refuse("cannot read the Cargo target directory");
      artifactFile = path.join(targetDir, "package", `${unit}-${version}.crate`);
      if (!fs.existsSync(artifactFile)) refuse(`cargo package did not produce ${artifactFile}`);
    }
    const artifactSha256 = sha256(fs.readFileSync(artifactFile));

    // 4. Evidence in the normal schema, then the EXISTING gate.
    const commands = [...plan.steps.map((s) => `${s.command} ${s.args.join(" ")}`), ...packCommands];
    const timestamp = (deps.now ?? (() => new Date().toISOString()))();
    const evidence = {
      schema: GATE_SCHEMA, kind, name: unit, version, commit: head, artifactSha256,
      hostPolicy: "darwin-only", hostPolicyReason: LOCAL_HOST_POLICY_REASON,
      runs: [{
        host: { os: "darwin", arch: deps.arch ?? process.arch }, commit: head, features: plan.featureLabels, artifactSha256,
        command: commands.join(" && "), operator: LOCAL_OPERATOR, timestamp,
        passed: verdict.executed, failed: 0, skipped: verdict.skipped, ignored: verdict.ignored, allowIgnored,
        requireAssets: env.RIGHTKIT_REQUIRE_ASSETS, assetGated: isAssetGated,
        steps: steps.map(({ command, ok, executed, failed, ignored, skipped, binaries }) => ({ command, ok, ...(executed === undefined ? {} : { executed, failed, ignored, skipped, binaries }) })),
      }],
    };
    const evidencePath = path.resolve(defaultEvidencePath(workspaceRoot, kind, unit));
    fs.mkdirSync(path.dirname(evidencePath), { recursive: true });
    fs.writeFileSync(evidencePath, `${JSON.stringify(evidence, null, 2)}\n`);
    log(`[right-release] wrote ${evidencePath} (artifact sha256 ${artifactSha256})`);
    const gateOptions = { kind, name: unit, version, packagePath: unitPath, evidencePath };
    (deps.enforceGate ?? enforcePublicationGate)(gateOptions, { write: (line) => log(line.replace(/\n$/, "")) });

    // 5. The EXISTING publish path.
    if (kind === "npm") {
      (deps.publishGatedTarball ?? publishGatedTarball)({ name: unit, version, tarball: artifactFile, evidencePath }, deps.publishDeps ?? {});
    } else {
      const published = await (deps.runCargoPublish ?? runCargoPublish)({ crate: unit, dryRun: false, allowDirty: false, workspaceRoot, evidence: evidencePath }, undefined, deps.cargoDeps ?? {});
      (deps.recordPublished ?? recordPublished)({ registry: "cargo", name: published.name, version: published.version });
    }
    log(`[right-release] published ${unit}@${version}`);
    return { kind, name: unit, version, commit: head, artifactSha256, evidencePath };
  } finally {
    cleanup();
  }
}

const realOrSelf = (file) => { try { return fs.realpathSync(file); } catch { return path.resolve(file); } };
if (process.argv[1] && realOrSelf(process.argv[1]) === realOrSelf(fileURLToPath(import.meta.url))) {
  const [kind, ...rest] = process.argv.slice(2);
  if (kind !== "npm" && kind !== "cargo") { process.stderr.write("right-release publish --local: expected npm or cargo\n"); process.exitCode = 2; }
  else runLocalPublish(kind, rest).catch((error) => { process.stderr.write(`right-release publish ${kind} --local: ${error.message}\n`); process.exitCode = 1; });
}
