#!/usr/bin/env node
import { expandPackageRecipeTarget } from "./package-recipe-targets.mjs";
import { access, readFile, readdir } from "node:fs/promises";
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, readdirSync, realpathSync, rmSync, unlinkSync, writeFileSync } from "node:fs";
import { buildLaunch } from "./build-launch.mjs";
import { createRequire } from "node:module";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { spawn, spawnSync } from "./background-process.mjs";
import { validateRightKitCargoContract } from "./cargo-contract.mjs";
import { assertCargoProfileContract } from "./cargo-profile-contract.mjs";
import { assertQaAdoption, assertQaBackdoorContract } from "./qa-contract.mjs";
import { assertLegalReleaseContract } from "./legal-contract.mjs";
import { DECLARED_UNRELATED_POLICY, declaredUnrelatedRefusal, releaseArtifactPaths } from "./hosted-workflow-policy.mjs";
import { assertNativeIosPrepareHasManifest, assertPrimaryReleaseCheckout, dirtyBuildInputs, resolveReleaseBuildInputs } from "./release-invocation.mjs";
import { resolveReleaseSource, sourceRootOf } from "./release-source-identity.mjs";
import { ArtifactPathError, assertSplitCommandPaths, releaseCommandSteps, resolveArtifactPath, splitArtifactContext } from "./release-artifact-path.mjs";
import { declaredCargoManifest, hasDefaultTauriManifest, resolveNativeCargoLayout } from "./native-cargo-layout.mjs";
import {
  canonicalPackageIdentity,
  createNativeFinalizationReceipt,
  mintNativeProvenance,
  resolveNativeFinalizerConfig,
  validateNativeFinalizationOutput,
} from "./native-release-finalization.mjs";
import { packageCommandOptional, resolveProjectType, targetUsesMsix } from "./project-type.mjs";
import { MSIX_SIGNING_CONTRACT, assertValidMsixTarget } from "./msix-config.mjs";
import { buildMsix } from "./msix-build.mjs";
import { msixBuildOptions } from "./msix-target.mjs";
import { rightKitVersionAllowed } from "./rightkit-version-policy.mjs";
import { RightKitRegistryError, SNAPSHOT_ENV, loadAdmittedSnapshot, prepareRightKitResolution, rightKitLockInventory } from "./rightkit-registry-resolver.mjs";
import { collectPreflight, formatPreflight, preflightFailures } from "./preflight.mjs";
import { patchTauriBundleType } from "./tauri-bundle-marker.mjs";
import { verifyNsisEmbeddedBinary } from "./nsis-payload.mjs";
import { terminateProcessTree } from "./heavy-command.mjs";
import { materializeHardeningEvidence } from "./hardening-evidence.mjs";
import { signingReceiptPath } from "./signing-receipts.mjs";
import { MEDIA_SIDECAR_OFFER_NAME, MEDIA_SIDECAR_RECEIPT_NAME, buildMediaSidecarReceipt, macSidecarSigningProblems, mediaSidecarNestedCode, planMediaSidecar, sidecarFileEvidence, validateMediaSidecarConfig, verifyStagedSidecars, writeMediaSidecarNotices } from "./media-sidecar.mjs";
import { loadSidecarCatalog } from "./media-sidecar-catalog.mjs";
import { EXTERNAL_BIN_RECEIPT_NAME, buildExternalBinReceipt, externalBinNestedCode, planExternalBin, validateExternalBinConfig, verifyStagedExternalBins } from "./external-bin.mjs";
import { notarizeAppBundle, validateNotarizationReceipt } from "./macos-notarization.mjs";
import { MACOS_PORTABLE_SIGNING_CONTRACT } from "./macos-signing-contract.mjs";
import { fileEvidence } from "./macos-file-evidence.mjs";
import { readDmgSpec } from "./dmg-spec.mjs";
import { hasNonDefaultSettings, matrixReceiptProblems, planMacSigning, validateMacSigningConfig } from "./mac-signing-matrix.mjs";
import { buildStapledSwiftMacDmg, swiftMacDmgConfigError, swiftMacDmgPlan } from "./swift-mac-dmg.mjs";
import { runConfiguredChecks } from "./release-checks.mjs";
import { resolveMacosDeveloperIdIdentity } from "./macos-signing-identity.mjs";
import { DEFAULT_STORAGE_THRESHOLD_BYTES, findInstallerCopyScripts, findLargeRepositorySubdirectories } from "./installer-storage-doctor.mjs";
import { macosCodesignEnvironment } from "./codesign-environment.mjs";
import { assertNoQaControl, assertNoWebBundleMocks } from "./qa-control-scan.mjs";
import { assertReleaseDependencies } from "./release-dependencies.mjs";
import { assertNoForbiddenBuildFeatures } from "./qa-build-features.mjs";
import { expandRecipeBuildCommands } from "./recipe-build.mjs";
import { assertNoReleaseSwitches, commandTokens, localCrateDirsFromTree, resolveFeatureClosure, shippedRustTriples, validateNonShipped } from "./release-switch-scan.mjs";
import { iosOwnershipScope, preflightOwnershipScope } from "./ownership-scope.mjs";
import { devtoolsEvidence, resolveDevtoolsAllowance } from "./devtools-policy.mjs";
import { runAndroidRelease, validateAndroidTarget } from "./android-release.mjs";
import { runIosPackageRecipe } from "./ios-package-recipe.mjs";
import { releaseSwitchInputs } from "./ios-build-context.mjs";
import { productionBuildEnv, productionModeProblems } from "./production-mode.mjs";
import { evaluatePrebuildGates } from "./prebuild-gates.mjs";
import { defaultReleaseConfigFile, importReleaseConfig, releaseConfigFieldProblems } from "./release-config.mjs";
import { describeRegistryError, formatNotRun, runAdmission } from "./admission.mjs";
import { sccacheRefusal } from "./admission-host.mjs";

const TOOL_ROOT = path.dirname(fileURLToPath(import.meta.url));
const HARDENING_SCAN = path.resolve(TOOL_ROOT, "hardeningscan.mjs");
const UPLOAD_LARGE = path.resolve(TOOL_ROOT, "upload-large.mjs");
const SIGN_WINDOWS = path.resolve(TOOL_ROOT, "sign-windows.mjs");
const SIGN_MACOS = path.resolve(TOOL_ROOT, "sign-macos.mjs");
const NOTARIZE_MACOS = path.resolve(TOOL_ROOT, "notarize-macos.mjs");
const SIGN_UPDATER = path.resolve(TOOL_ROOT, "sign-updater.mjs");
const RIGHTKIT_VERSIONS = JSON.parse(readFileSync(path.resolve(TOOL_ROOT, "rightkit-versions.json"), "utf8"));
// The running package's own version: the bundled feed lags by one release (it is written after the publish).
const VERSION = JSON.parse(readFileSync(path.resolve(TOOL_ROOT, "package.json"), "utf8")).version;
const TIERS = new Set(["patch", "update"]);
const RIGHTKIT_ALLOWED = buildAllowedVersions(
  RIGHTKIT_VERSIONS.npm,
  RIGHTKIT_VERSIONS.stagedNpm,
  RIGHTKIT_VERSIONS.legacyNpm,
  RIGHTKIT_VERSIONS.registry?.npm,
);
const RIGHTKIT_CARGO_ALLOWED = buildAllowedVersions(
  RIGHTKIT_VERSIONS.cargo,
  RIGHTKIT_VERSIONS.stagedCargo,
  RIGHTKIT_VERSIONS.legacyCargo,
  RIGHTKIT_VERSIONS.registry?.cargo,
);
const RIGHTKIT_OWNERSHIP = JSON.parse(readFileSync(path.resolve(TOOL_ROOT, "rightkit-ownership.json"), "utf8"));
// Exact RightKit versions proven against the canonical registries for THIS app's locks (see rightkit-registry-resolver.mjs).
// Exact membership only: a verified version never widens the bundled min..max range.
let rightKitVerifiedNpm = new Map();
const FORBIDDEN_RIGHTKIT_SPEC = /^(?:git|file|link|workspace):|github|github\.com/i;
const WINDOWS_NSIS_SIGNING_CONTRACT = "windows-raw-exe-authenticode-before-nsis-v1";
const WINDOWS_PORTABLE_SIGNING_CONTRACT = "windows-raw-exe-authenticode-before-portable-v1";
const DEFAULT_NOTARIZATION_TIMEOUT_MS = 30 * 60 * 1000;

// Apple Developer ID and notary secrets: the only secrets the signing jobs of the managed release chain
// (macos-sign, and dev-mac-sign when devArtifact.mac is "notarize") may reference.
const APPLE_RELEASE_SECRETS = ["APPLE_CERTIFICATE_BASE64", "APPLE_CERTIFICATE_PASSWORD", "APPLE_KEYCHAIN_PASSWORD", "APPLE_API_KEY_BASE64", "APPLE_API_KEY", "APPLE_API_ISSUER"];
// Package-manager publication tokens of the managed publish-package-managers lane.
const PACKAGE_MANAGER_PUBLISH_SECRETS = ["HOMEBREW_TAP_TOKEN", "WINGET_CREATE_GITHUB_TOKEN"];

// @rightkit/ci `swift.privatePackages` (0.2.41) renders one configure step and one cleanup step, with the
// token passed through step `env` as `RIGHT_GIT_PRIVATE_TOKEN_<n>: ${{ secrets.<NAME> }}`, into exactly these
// workflow/job pairs. Nothing else may reference the declared secret: not another job of these workflows, not
// package-smoke, publish-npm or publish-package-managers, and not a signing, qualification or publish job.
const SWIFT_PRIVATE_STEP_NAMES = new Set(["Configure private Swift package access", "Remove private Swift package access"]);
const SWIFT_PRIVATE_ANY_PROFILE = { only: "any profile", allows: () => true };
const SWIFT_PRIVATE_NODE_PROFILE = { only: "the node profile", allows: (manifest) => (manifest?.profile ?? "node") === "node" };
const SWIFT_PRIVATE_PARALLEL_DEV_MAC = {
  only: "profile rust-hybrid with devArtifact.parallel and a devArtifact.mac of sign, notarize or unsigned",
  allows: (manifest) => manifest?.profile === "rust-hybrid"
    && manifest?.devArtifact?.parallel === true
    && ["sign", "notarize", "unsigned"].includes(manifest.devArtifact.mac),
};
const SWIFT_PRIVATE_JOBS = new Map([
  ["ci.yml", new Map([["ci", SWIFT_PRIVATE_ANY_PROFILE], ["dev-artifact-mac", SWIFT_PRIVATE_PARALLEL_DEV_MAC], ["test", SWIFT_PRIVATE_NODE_PROFILE]])],
  ["swift-check.yml", new Map([["swift-check", SWIFT_PRIVATE_ANY_PROFILE]])],
  ["qualification.yml", new Map([["qualification", SWIFT_PRIVATE_ANY_PROFILE]])],
  ["release-candidate.yml", new Map([["candidate", SWIFT_PRIVATE_ANY_PROFILE]])],
  ["regenerate.yml", new Map([["regenerate", SWIFT_PRIVATE_ANY_PROFILE]])],
]);
// release-candidate.yml and regenerate.yml are dispatch lanes by design. In ci.yml and qualification.yml a
// workflow_dispatch trigger would let a manual run of any ref reach the credential.
const SWIFT_PRIVATE_DISPATCH_WORKFLOWS = new Set(["release-candidate.yml", "regenerate.yml"]);

const args = process.argv.slice(2);
const opts = {
  config: defaultReleaseConfigFile(),
  platform: process.platform === "win32" ? "win" : process.platform === "darwin" ? "mac" : process.platform,
  upload: false,
  install: false,
  dryRun: false,
  skipChecks: false,
  doctor: false,
  gates: false,
  inspect: false,
  strict: false,
  tier: null,
  resume: false,
  resumeFrom: null,
};

// E01: every independent problem of one run is collected and reported together; the doctor and the build share it.
// `check` / `reject` record a finding and continue; a check whose input comes from a failed one is not run on that bad input
// but reported as `not run because <check> failed` (one line, not a finding: the failed check is the one blocking problem of that
// cause); a throw inside a check is a finding, never a crash. `fail` stays for what cannot continue (it prints what was
// collected first). The build stops at one checkpoint (before the release lock, install, checks and compile) when any finding
// exists, so no build step or other side effect ever follows a failed check.
const problems = []; // { name, detail }
const notRunLines = []; // what a failed check kept from running; reported beside the findings, never counted as one
const failedChecks = new Set();
// The three rows only the doctor owns (admission.mjs skips them in its own pass) are not admission findings, so nothing else puts
// their id before the text. The doctor does, as it does for every admission row: `right-release: <row>: <text>`, the text the
// build's controller prints after `right-release build: <row>: `.
const DOCTOR_OWNED_ROWS = new Set(["rightkit-registry", "build-inputs", "dirty-inputs"]);
function reject(name, detail) {
  failedChecks.add(name);
  problems.push({ name, detail: opts.doctor && DOCTOR_OWNED_ROWS.has(name) ? `${name}: ${detail}` : detail });
}
// A validator that runs inside a check() refuses with this instead of calling fail(): the check records it as a finding and the
// doctor goes on to the next rule. `rowId` names the rule; `problems` lists every finding the rule has when it has more than one.
class ReleaseRefusal extends Error {
  constructor(message, rowId, found = [message]) {
    super(message);
    this.name = "ReleaseRefusal";
    this.rowId = rowId;
    this.problems = found;
  }
}
function rejectBlocked(name, blockers, what = name) {
  const blocker = blockers.find((candidate) => failedChecks.has(candidate));
  if (!blocker) return false;
  // The check counts as failed for the checks that read it (they are not run either), but it adds no problem of its own.
  failedChecks.add(name);
  notRunLines.push(`${what} not run because ${blocker} failed`);
  return true;
}
async function check(name, after, run) {
  if (rejectBlocked(name, after)) return { ok: false };
  try {
    return { ok: true, value: await run() };
  } catch (error) {
    if (error instanceof ReleaseRefusal) for (const detail of error.problems) reject(name, detail);
    else reject(name, error instanceof Error ? error.message : `${name} threw a non-Error value: ${String(error)}`);
    return { ok: false };
  }
}
const printProblems = () => {
  for (const problem of problems.splice(0)) console.error(`right-release: ${problem.detail}`);
  for (const line of notRunLines.splice(0)) console.error(`right-release: ${line}`);
};

for (let i = 0; i < args.length; i++) {
  const arg = args[i];
  if (arg === "--") continue;
  else if (arg === "--config") opts.config = args[++i];
  else if (arg === "--platform") opts.platform = args[++i];
  else if (arg === "--upload" || arg === "--publish") opts.upload = true;
  else if (arg === "--no-upload" || arg === "--package") opts.upload = false;
  else if (arg === "--install") opts.install = true;
  else if (arg === "--dry-run") opts.dryRun = true;
  else if (arg === "--skip-checks") opts.skipChecks = true;
  else if (arg === "--skip-hardening") fail("--skip-hardening is forbidden; use exact artifact/source-bound hardeningAllowances");
  else if (arg === "--doctor") opts.doctor = true;
  else if (arg === "--gates") opts.gates = true;
  // The doctor applies the build's admission by default; --admission is the old spelling of that, kept so scripts still run.
  else if (arg === "--admission") opts.admission = true;
  else if (arg === "--inspect") opts.inspect = true;
  else if (arg === "--strict") opts.strict = true;
  else if (arg === "--resume") opts.resume = true;
  else if (arg === "--resume-from") { opts.resume = true; opts.resumeFrom = args[++i]; }
  else if (arg === "--tier") opts.tier = args[++i];
  else if (arg.startsWith("--tier=")) opts.tier = arg.slice("--tier=".length);
  else if (arg === "--version") {
    console.log(VERSION);
    process.exit(0);
  }
  else if (arg === "-h" || arg === "--help") usage(0);
  else usage(2, `unknown argument: ${arg}`);
}

if (opts.gates && !opts.doctor) usage(2, "--gates is valid only with --doctor");
if ((opts.inspect || opts.strict) && !opts.doctor) usage(2, `${opts.inspect ? "--inspect" : "--strict"} is valid only with --doctor`);
if (opts.inspect && opts.admission) usage(2, "--inspect and --admission contradict each other: --inspect lists newer RightKit versions as information, --admission refuses them");
if (opts.upload) fail("combined build+upload was removed; run right-release build, then right-release upload --release <id> --tier patch|update");
if (opts.tier && !TIERS.has(opts.tier)) usage(2, `invalid --tier: ${opts.tier} (expected patch|update)`);
const MAC_RESUME_PHASES = ["prepackage", "sign", "package", "post-sign", "harden"];
if (opts.resumeFrom && !MAC_RESUME_PHASES.includes(opts.resumeFrom)) usage(2, `invalid --resume-from: ${opts.resumeFrom}`);
const sccacheVersion = assertSharedSccachePrerequisite();

const requestedConfigPath = path.resolve(opts.config);
// Native: expands a Windows short name (RUNNER~1), so the path compares with Git's long root below.
const configPath = opts.doctor ? realpathSync.native(requestedConfigPath) : requestedConfigPath;
let doctorInvocation;
if (opts.doctor) {
  const invocationRoot = path.dirname(configPath);
  const repoRoot = git(invocationRoot, ["rev-parse", "--show-toplevel"]);
  assertPrimaryReleaseCheckout(repoRoot);
  const relativeConfig = path.relative(repoRoot, configPath).replaceAll("\\", "/");
  if (relativeConfig.startsWith("../")) fail("release config must live inside the Git repository");
  const commit = git(repoRoot, ["rev-parse", "HEAD"]);
  const dirtyConfig = dirtyBuildInputs(repoRoot, { include: [relativeConfig], required: [relativeConfig] }, commit);
  if (dirtyConfig.length > 0) fail(`dirty release config cannot be executed:\n${dirtyConfig.map((file) => `- ${file}`).join("\n")}`);
  doctorInvocation = { repoRoot, commit };
}
let config;
try { config = await importReleaseConfig(configPath); } catch (error) { fail(`release config could not be loaded (${configPath}): ${error instanceof Error ? error.message : String(error)}`); }
if (!config) usage(2, `config did not export default: ${configPath}`);
if (config.schema !== 1) fail(`unsupported config schema: ${config.schema ?? "<missing>"} (expected 1)`);
// Field-level config refusals (updater.keyFile: keys come from the OS credential provider) are listed with every other finding.
for (const refusal of releaseConfigFieldProblems(config)) reject(refusal.name, refusal.detail);

// Hosted-workflow policy patterns: defined before the top-level await below uses them.
const SIGN_EXISTING_SECRETS = new Set(["APPLE_CERTIFICATE_BASE64", "APPLE_CERTIFICATE_PASSWORD", "APPLE_KEYCHAIN_PASSWORD"]);
const SIGN_EXISTING_ACTIONS = new Set(["actions/checkout", "actions/setup-node", "pnpm/action-setup", "actions/download-artifact", "actions/upload-artifact"]);
const SIGN_EXISTING_RIGHT_RELEASE_COMMANDS = new Set(["ownership", "sign-dev-artifact"]);
const SIGN_EXISTING_BUILD_RE = /\b(?:cargo|rustc|xcodebuild|tauri|cmake|msbuild|gradle|notarytool|stapler)\b|swift build|right-release build|--release\b|\b(?:npm|pnpm|yarn) (?:run|build)\b/;
const SIGN_EXISTING_GUARD = "  private-repo-guard:\n    runs-on: ubuntu-24.04\n    steps:\n      - name: Refuse private repository\n        if: ${{ github.event.repository.private }}\n        run: exit 1\n";
const SIGN_EXISTING_JOB_HEAD = /^(?:    #[^\n]*\n)*    needs: \[private-repo-guard\]\n    if: \$\{\{ github\.event_name == 'workflow_dispatch' && github\.ref_name == github\.event\.repository\.default_branch \}\}\n    environment: [A-Za-z0-9][A-Za-z0-9._-]*\n    runs-on: (?:macos|xcode)-[A-Za-z0-9._-]+\n(?:    timeout-minutes: \d+\n)?    permissions:\n      contents: read\n      actions: read\n    steps:\n/;
// Head of the managed `supersede` job (the span starts at the preceding newline, before the job key line).
const SUPERSEDE_JOB_HEAD = /^\n  supersede:\n    needs: \[private-repo-guard\]\n    if: \$\{\{ github\.event_name == 'push' && github\.ref_name == github\.event\.repository\.default_branch \}\}\n    runs-on: ubuntu-[0-9.]+\n    timeout-minutes: \d+\n    permissions:\n      contents: read\n      actions: write\n    steps:\n/;

const root = path.dirname(configPath);
const workdir = path.resolve(root, config.workdir ?? ".");
// A native iOS app (xcodegen project; Rust built by iosPackage.prepare) with no src-tauri and no declared nativeAssembly
// manifest owns no Cargo layout: null, so the Cargo-derived preflight, sweep and package-contract steps are skipped for it.
const nativeIos = opts.platform === "ios" && !hasDefaultTauriManifest({ appRoot: root });
const nativeLayout = nativeIos ? null : resolveNativeCargoLayout({ appRoot: root, config });
const nativeAssembly = config.nativeAssembly;
const nativeFinalizer = resolveNativeFinalizerConfig(nativeAssembly);
// RightKit pins are proven against the canonical registries once per release, at build admission by the controller
// (build-release.mjs). This worker, a resume and the upload consume only that admitted snapshot, through the release's own
// state root and admission record; a snapshot from anywhere else is refused. The standalone doctor resolves live.
const rightKitSnapshotFile = process.env[SNAPSHOT_ENV] || null;
let rightKitResolution = null; // null when the registry resolution failed (recorded as the `rightkit-registry` finding)
// The worker of a door split (row 7.04): its cwd is the snapshot, which has no git directory. Its commit and source root are the selected
// commit's and the snapshot's (release-source-identity.mjs); the checkout route reads them from git as before.
let workerSource = null;
let artifactPathContext = null; // the declared-artifact resolver's context (artifactContext, row 7.04c)
if (!opts.doctor && process.env.RK_GATE_CHECKOUT) {
  try { workerSource = resolveReleaseSource({ cwd: process.cwd() }); } catch (error) { fail(`${error.code ?? "RK_SOURCE_IDENTITY"}: ${error.message}`); }
}
try {
  const lockInventory = () => {
    try {
      const repoRoot = workerSource ? sourceRootOf(workerSource) : git(root, ["rev-parse", "--show-toplevel"]);
      let canonicalTarget = config.targets?.[opts.platform];
      try { canonicalTarget = expandPackageRecipeTarget(canonicalTarget, { platform: opts.platform, appRoot: root, config }); } catch { /* reported later */ }
      const inputs = resolveReleaseBuildInputs({ repoRoot, appRoot: root, configPath, config, target: canonicalTarget });
      return rightKitLockInventory({ requiredInputs: inputs.requiredInputs, cargoLockPaths: inputs.cargoLockPaths, nativeLayout: inputs.nativeLayout, config, root });
    } catch {
      return rightKitLockInventory({ nativeLayout, config, root });
    }
  };
  let boundSnapshot = null;
  if (!opts.doctor) {
    if (!rightKitSnapshotFile) throw new RightKitRegistryError("no-admission", "no admitted RightKit registry snapshot was handed to the release worker; run it through `right-release build`, which admits the release");
    const stateRoot = process.env.RIGHT_RELEASE_STATE_ROOT;
    if (!stateRoot) throw new RightKitRegistryError("no-admission", "RIGHT_RELEASE_STATE_ROOT is not set; the admitted RightKit registry snapshot cannot be located");
    const commit = workerSource ? workerSource.commit : git(root, ["rev-parse", "HEAD"]);
    boundSnapshot = loadAdmittedSnapshot(stateRoot, { releaseId: `${config.app}-${config.version}-${commit.slice(0, 8)}`, commit, platform: opts.platform, snapshotFile: rightKitSnapshotFile });
  }
  rightKitResolution = await prepareRightKitResolution({
    root,
    extraLocks: lockInventory(),
    versions: RIGHTKIT_VERSIONS,
    ownership: RIGHTKIT_OWNERSHIP,
    releaseVersion: VERSION,
    boundSnapshot,
    // The doctor is the build's own admission: a pin behind latest is refused, as the build refuses it. --inspect is the
    // informational view (newer versions are listed, not refused).
    admission: opts.doctor && !opts.inspect,
  });
} catch (error) {
  rightKitResolution = null;
  reject("rightkit-registry", describeRegistryError(error, config.app ?? path.basename(root)));
}
if (rightKitResolution) {
  const { newer, tasks, warnings, notes } = rightKitResolution.freshness;
  const out = opts.doctor ? console.log : console.error;
  for (const line of newer) out(`right-release: newer RightKit version (informational): ${line}`);
  for (const line of tasks) out(`right-release: LANE TASK ${line}`);
  for (const line of warnings) console.error(`right-release: WARNING ${line}`);
  for (const line of notes) console.error(`right-release: note ${line}`);
}
if (rightKitResolution) rightKitVerifiedNpm = rightKitResolution.verified.npm;
// The contracts below are independent of each other; the two that read the proven RightKit versions need the registry resolution.
// A SwiftPM app or a native iOS app without a package.json has no Node package contract to validate. Each package rule is its
// own finding (a failed one blocks only the rules that read its result), so one run lists every violation.
if (!((nativeIos || resolveProjectType({ appRoot: root, config, target: config.targets?.[opts.platform] }) === "swiftpm") && !existsSync(path.join(root, "package.json")))) {
  const packageJson = await check("package-json", [], async () => JSON.parse(await readFile(path.join(root, "package.json"), "utf8")));
  const packageLabel = () => config.app ?? packageJson.value?.name ?? "app";
  await check("qa-backdoor-contract", ["package-json"], () => assertQaBackdoorContract(root, packageJson.value.scripts ?? {}));
  await check("hosted-workflows", [], () => validateHostedWorkflowFiles(root, packageLabel(), config.hostedWorkflows, releaseArtifactPaths(config)));
  await check("package-manager", ["package-json"], () => validatePackageManagerPin(packageJson.value, packageLabel()));
  await check("package-workspace-scripts", ["package-json"], () => validateNoParentWorkspaceScripts(packageJson.value, packageLabel()));
  await check("rightkit-pin-specs", ["package-json", "rightkit-registry"], () => validateRightKitPinSpecs(packageJson.value, packageLabel()));
  await check("rightkit-release-supported", ["package-json", "rightkit-registry"], () => validateRightKitReleaseSupported(packageJson.value, packageLabel()));
}
await check("rightkit-cargo-contract", ["rightkit-registry"], () => {
  validateRightKitCargoContract(root, mergeVerifiedVersions(RIGHTKIT_CARGO_ALLOWED, rightKitResolution.verified.cargo), config.app ?? path.basename(root));
});
// One standard dev profile and panic = "unwind" in every app (templates/cargo-profiles.toml).
await check("cargo-profile-contract", [], () => assertCargoProfileContract(root, config.app ?? path.basename(root)));
// RightKit QA is mandatory for every app (also SwiftPM apps without a package.json): no QA lane refuses the release.
await check("qa-adoption", [], () => assertQaAdoption({ appRoot: root, app: config.app }));
let target = config.targets?.[opts.platform];
// Declarative winPackage / macPackage (#345) expand to the same hook shape every gate below already checks.
// When it fails, `target` is not usable: every check that reads it is reported as not run because `target` failed.
await check("target", [], () => {
  let expanded;
  try { expanded = expandPackageRecipeTarget(target, { platform: opts.platform, appRoot: root, config }); } catch (error) { throw new Error(`${config.app ?? "app"} ${opts.platform} ${error.message}`); }
  if (!expanded) throw new Error(`${config.app ?? "app"} has no ${opts.platform} release target`);
  target = expanded;
});
// Row 7.04c (R-T1, R-T4): in a door split each Cargo manifest's target is bound now, once, before any step runs, and no step the app owns may name
// a target-relative path (the snapshot has no target directory). Checkout mode checks nothing here.
await check("split-artifact-paths", ["target"], () => {
  if (!workerSource) return;
  assertSplitCommandPaths(releaseCommandSteps({ target, config, root }), artifactContext());
});
const legalCheck = await check("legal-contract", [], () => (config.legal
  ? assertLegalReleaseContract(root, config.legal, config.app ?? path.basename(root), opts.platform)
  : null));
const legalContract = legalCheck.ok ? legalCheck.value : null;
let macSigning; // validated macOS sign.prePackageFiles / sign.nestedCode (mac only)
let mediaSidecarConfig = null; // validated targets.<mac|win>.mediaSidecar (rightkit-ffmpeg decode sidecar)
let externalBinConfig = null; // validated targets.<mac|win>.externalBin (generic Tauri externalBin sidecars, #270)
let androidTarget; // validated Android AAB target (android only; see android-release.mjs)
let iosSpec; // validated iOS iosPackage spec (ios only; package-recipe-targets.mjs expands targets.ios, ios-package-recipe.mjs runs it)
const app = config.app ?? "app";
// Everything below reads the expanded target. Each independent refusal is its own finding; a check that needs the output of
// another (mac signing reads the sidecar config, the iOS prepare check reads the iOS spec) is not run (reported as such), never run on it.
if (!rejectBlocked("target-checks", ["target"], "the target checks (signed, mediaSidecar, externalBin, publishBlocked and the platform signing and package contract)")) {
  if (target.signed !== true) reject("target-signed", `${app} ${opts.platform} must declare signed: true; unsigned release targets are forbidden`);
  if (target.mediaSidecar !== undefined) {
    if (opts.platform !== "mac" && opts.platform !== "win") reject("media-sidecar", `${app} ${opts.platform} does not support mediaSidecar (mac and win only)`);
    else await check("media-sidecar", [], () => {
      try { mediaSidecarConfig = validateMediaSidecarConfig(target.mediaSidecar, { platform: opts.platform }); } catch (error) { throw new Error(`${app} ${opts.platform} ${error.message}`); }
    });
  }
  if (target.externalBin !== undefined) {
    if (opts.platform !== "mac" && opts.platform !== "win") reject("external-bin", `${app} ${opts.platform} does not support externalBin (mac and win only)`);
    else await check("external-bin", ["media-sidecar"], () => {
      try { externalBinConfig = validateExternalBinConfig(target.externalBin, { platform: opts.platform, reservedNames: mediaSidecarConfig?.tools ?? [] }); } catch (error) { throw new Error(`${app} ${opts.platform} ${error.message}`); }
    });
  }
  if (opts.platform === "win") {
    const isNsis = target.signingContract === WINDOWS_NSIS_SIGNING_CONTRACT;
    const isPortable = target.signingContract === WINDOWS_PORTABLE_SIGNING_CONTRACT;
    const isMsix = target.signingContract === MSIX_SIGNING_CONTRACT;
    if (!isNsis && !isPortable && !isMsix) reject("win-signing-contract", `${app} win must declare signingContract: ${WINDOWS_NSIS_SIGNING_CONTRACT}, ${WINDOWS_PORTABLE_SIGNING_CONTRACT} or ${MSIX_SIGNING_CONTRACT}`);
    if (isMsix) {
      await check("win-msix", [], () => assertValidMsixTarget(target, app));
    } else {
      // The NSIS and portable rules below depend on which contract the target declares.
      rejectBlocked("win-contract-rules", ["win-signing-contract"], "the win NSIS and portable rules (sign.files, distinct raw EXE and installer files)");
      if (!target.prePackage?.cmd) reject("win-prepackage", `${app} win must declare prePackage for the raw EXE build`);
      const rawFiles = target.sign?.prePackageFiles;
      if (!rawFiles?.length) reject("win-sign-prepackage-files", `${app} win must declare at least one sign.prePackageFiles raw EXE`);
      else {
        if (rawFiles.some((file) => typeof file !== "string") || target.sign.nestedCode !== undefined) reject("win-sign-entries", `${app} win sign.prePackageFiles entries must be path strings; object entries and sign.nestedCode are macOS-only`);
        if (isNsis && rawFiles.length !== 1) reject("win-nsis-raw-exe", `${app} win NSIS must declare exactly one sign.prePackageFiles raw EXE`);
      }
      if (isNsis && !target.sign?.files?.length) reject("win-nsis-sign-files", `${app} win NSIS must declare sign.files for installer signing`);
      if (isNsis && rawFiles?.length && target.sign?.files?.length && rawFiles.some((file) => target.sign.files.includes(file))) reject("win-distinct-files", `${app} win raw EXE and installer signing files must be distinct`);
      if (isPortable && target.sign?.files?.length) reject("win-portable-sign-files", `${app} win portable release must sign every executable through sign.prePackageFiles and omit sign.files`);
    }
  }
  if (opts.platform === "mac") {
    if (target.signingContract !== MACOS_PORTABLE_SIGNING_CONTRACT) reject("mac-signing-contract", `${app} mac must declare signingContract: ${MACOS_PORTABLE_SIGNING_CONTRACT}`);
    if (!target.prePackage?.cmd) reject("mac-prepackage", `${app} mac must declare prePackage to materialize Developer ID signing files`);
    if (!target.sign?.prePackageFiles?.length) reject("mac-sign-prepackage-files", `${app} mac must declare at least one sign.prePackageFiles`);
    else if (!rejectBlocked("mac-signing", ["media-sidecar", "external-bin"], `${app} mac signing config validation (it reads the sidecar and externalBin configs)`)) {
      await check("mac-signing", [], () => {
        const macSignConfig = mediaSidecarConfig || externalBinConfig
          ? { ...target.sign, nestedCode: { ...(mediaSidecarConfig ? mediaSidecarNestedCode(mediaSidecarConfig) : {}), ...(externalBinConfig ? externalBinNestedCode(externalBinConfig) : {}), ...(target.sign.nestedCode ?? {}) } }
          : target.sign;
        try { macSigning = validateMacSigningConfig(macSignConfig, { root }); } catch (error) { throw new Error(`${app} mac ${error.message}`); }
      });
    }
    if (!target.notarize?.file) reject("mac-notarize-file", `${app} mac must declare notarize.file`);
    else {
      if (target.notarize.appBeforePackage && !target.notarize.app) reject("mac-notarize-app", `${app} mac notarize.appBeforePackage requires notarize.app`);
      if (target.notarize.staple === false && (target.notarize.app || /\.(dmg|pkg)$/i.test(target.notarize.file))) {
        reject("mac-notarize-staple", `${app} mac notarize.staple: false applies only to an archive of bare executables (no notarize.app, not a .dmg/.pkg)`);
      }
    }
    await check("mac-swift-dmg", [], () => {
      const swiftDmgError = swiftMacDmgConfigError({ target, projectType: resolveProjectType({ appRoot: root, config, target }), appName: app });
      if (swiftDmgError) throw new Error(swiftDmgError);
    });
    if (target.dmg && typeof target.dmg === "object" && target.dmg.spec !== undefined && !rejectBlocked("mac-dmg-spec", ["mac-notarize-file"], `${app} mac targets.mac.dmg.spec validation (it reads notarize.app)`)) {
      await check("mac-dmg-spec", [], () => {
        try {
          if (typeof target.dmg.spec !== "string" || !target.dmg.spec) throw new Error("dmg.spec must be a path string");
          const specOptions = readDmgSpec(path.resolve(root, target.dmg.spec), { app: target.notarize.app });
          for (const asset of [specOptions.background, specOptions.volumeIcon]) if (asset && !existsSync(asset)) throw new Error(`referenced file not found: ${asset}`);
        } catch (error) { throw new Error(`${app} mac targets.mac.dmg.spec: ${error.message}`); }
      });
    }
  }
  if (opts.platform === "android") {
    await check("android-target", [], () => {
      try { androidTarget = validateAndroidTarget(target, { root }); } catch (error) { throw new Error(`${app} android ${error.message}`); }
    });
  }
  if (opts.platform === "ios") {
    if (target.recipe?.kind !== "iosPackage") reject("ios-target", `${app} ios must declare targets.ios.iosPackage; there is no hand-written iOS package script`);
    else {
      iosSpec = target.recipe.spec;
      if (nativeIos) await check("ios-prepare-manifest", [], () => {
        try { assertNativeIosPrepareHasManifest(iosSpec, config, root); } catch (error) { throw new Error(`${app} ios ${error.message}`); }
      });
    }
  }
  if (opts.upload && target.publishBlocked) reject("publish-blocked", `${app} ${opts.platform} publish blocked: ${target.publishBlocked}`);
}
/**
 * Assemble preflight inputs from the app's own files. Kept here (not in
 * preflight.mjs) so the check module stays free of release.mjs's config shape.
 */
function releasePreflight({ config, configPath, root, repoRoot, platform, nativeLayout: suppliedNativeLayout = nativeLayout, projectTarget = config.targets?.[platform], pinnedCargoLocks = [] }) {
  let version;
  try {
    version = JSON.parse(readFileSync(path.join(root, "package.json"), "utf8")).version;
  } catch {
    version = undefined;
  }
  // Every Cargo.lock the build compiles from; a vendored-OpenSSL edge in any of
  // them means the build needs a real Windows Perl.
  const cargoLockPaths = [
    ...(suppliedNativeLayout?.lockCandidates ?? []),
    ...pinnedCargoLocks,
    ...(config.cargoLocks ?? []).map((entry) => path.resolve(root, entry)),
  ].filter((entry, index, entries) => entries.indexOf(entry) === index);
  // Same scope the build uses (rightkit#927): an iOS target is checked over its Xcode project and local packages only.
  // The doctor reports a refused scope as an ownership FAIL (never a crash, never a whole-repo scan); the build still throws.
  let ownershipScope = null;
  let ownershipScopeError = null;
  try {
    ownershipScope = preflightOwnershipScope({ repoRoot, appRoot: root, platform, target: projectTarget, nativeAssembly: config.nativeAssembly });
  } catch (error) {
    ownershipScopeError = error instanceof Error ? error.message : String(error);
  }
  return collectPreflight({
    platform,
    ownershipScope,
    ownershipScopeError,
    appRoot: root,
    repoRoot,
    app: config.app,
    version,
    configPath,
    cargoLockPaths,
    nativeLayout: suppliedNativeLayout,
    targetLink: suppliedNativeLayout?.targetLink,
    projectType: resolveProjectType({ appRoot: root, config, target: projectTarget }),
    rust: Boolean(suppliedNativeLayout) || pinnedCargoLocks.length > 0,
    bridge: Boolean(suppliedNativeLayout),
    msix: targetUsesMsix(projectTarget),
  });
}

if (opts.doctor) {
  // Every check below is independent unless it names what it needs; a failed input blocks (never feeds) what reads it.
  const inputs = await check("build-inputs", ["target"], () => resolveReleaseBuildInputs({
    repoRoot: doctorInvocation.repoRoot,
    appRoot: root,
    configPath,
    config,
    target,
  }));
  await check("dirty-inputs", ["build-inputs"], () => {
    const dirtyInputs = dirtyBuildInputs(doctorInvocation.repoRoot, inputs.value.buildInputs, doctorInvocation.commit);
    if (dirtyInputs.length > 0) throw new Error(`dirty files can change the packaged app; resolve them before release:\n${dirtyInputs.map((file) => `- ${file}`).join("\n")}`);
  });
  // The build's own admission rows (admission.mjs): the files the build would read and this machine, run with the texts and the
  // rows the build runs before it takes its lock. The registry, the build inputs and the dirty inputs are this doctor's own steps.
  let notRun = [];
  const admission = await check("admission", ["target"], () => runAdmission({
    root, repoRoot: doctorInvocation.repoRoot, configPath, config, platform: opts.platform, target, tiers: ["files", "host"],
    mode: "doctor", skipDoctorOwned: true, skipChecks: opts.skipChecks, commit: doctorInvocation.commit,
  }));
  if (admission.ok) {
    for (const finding of admission.value.findings) reject(finding.row, `${finding.row}: ${finding.detail}`);
    notRun = admission.value.notRun;
  }
  console.log(`right-release ${VERSION}`);
  console.log(`config: ${configPath}`);
  console.log(`app: ${config.app}`);
  console.log(`platform: ${opts.platform}`);
  console.log(`tier: ${opts.tier ?? "<required for release/publish>"}`);
  console.log(`packageManager: ${config.packageManager}`);
  if (rightKitResolution) console.log(`rightkit-registry: ${rightKitResolution.summary}`);
  console.log(`workdir: ${workdir}`);
  console.log(`hardeningscan: ${HARDENING_SCAN}`);
  if (sccacheVersion) console.log(`sccache: ${sccacheVersion}`);
  if (legalContract) {
    console.log(`legal: ${legalContract.manifestPath}`);
    console.log(`legalAcceptance: ${legalContract.acceptanceVersion}`);
    console.log(`legalManifestSha256: ${legalContract.manifestSha256}`);
  }
  if (!failedChecks.has("target")) {
    if (target.sign?.files?.length) console.log(`sign: ${target.sign.files.join(", ")}`);
    if (target.publishBlocked) console.log(`publishBlocked: ${target.publishBlocked}`);
  }
  const managedRoot = process.env.RIGHTKIT_MANAGED_ROOT;
  await check("artifact-storage", [], () => {
    const installerCopyFindings = findInstallerCopyScripts({
      appRoot: root,
      workspaceRoot: process.env.RIGHTKIT_WORKSPACE_ROOT ?? path.resolve(doctorInvocation.repoRoot, ".."),
    });
    const storageFindings = [
      ...installerCopyFindings,
      ...findLargeRepositorySubdirectories({ appRoot: root, managedRoot, declared: config.storage?.declaredStores ?? [], thresholdBytes: Number(process.env.RIGHTKIT_ARTIFACT_STORAGE_THRESHOLD_BYTES ?? DEFAULT_STORAGE_THRESHOLD_BYTES) }),
    ];
    if (storageFindings.length) {
      console.log("artifact-storage: report-only storage findings detected:");
      for (const finding of storageFindings) console.log(`  - ${finding.path}: ${finding.reason}`);
      throw new Error(`artifact-storage found ${storageFindings.length} repository storage finding(s); move outputs to RIGHT_RELEASE_ARTIFACT_DIR`);
    } else console.log("artifact-storage: no installer-copy scripts detected");
  });

  // Config above, MACHINE below. Printing config never told anyone whether this
  // box could finish a build; these checks do. The machine preflight reads the resolved build inputs, so it is
  // BLOCKED when those could not be resolved.
  let failures = [];
  const machine = await check("preflight", ["target", "build-inputs"], () => {
    const { nativeLayout: resolvedNativeLayout, cargoLockPaths: pinnedCargoLocks } = inputs.value;
    return releasePreflight({ config, configPath, root, repoRoot: doctorInvocation.repoRoot, platform: opts.platform, nativeLayout: resolvedNativeLayout, pinnedCargoLocks });
  });
  if (machine.ok) {
    console.log("preflight:");
    console.log(formatPreflight(machine.value));
    failures = preflightFailures(machine.value);
  }
  // --gates: run the build's pre-build source gates (release-switch scan, feature closure, release dependencies,
  // devtools, production mode) over this app and report each finding as a FAIL row. They read the expanded target
  // (and, on iOS, the iOS spec), so a failed one of those blocks them.
  let gateFindings = [];
  if (opts.gates) {
    const blocker = ["target", ...(opts.platform === "ios" ? ["ios-target"] : [])].find((name) => failedChecks.has(name));
    gateFindings = blocker
      ? [{ name: "prebuild-gates", detail: `BLOCKED by ${blocker}` }]
      : evaluatePrebuildGates({ root, config, target, platform: opts.platform, iosSpec, nativeIos, nativeAssembly, nativeFinalizer, strict: !opts.dryRun }).findings;
  }
  const configProblems = problems.length;
  // The config findings (stderr, as every early refusal always was) and the gate rows (stdout) of this one run.
  printProblems();
  if (gateFindings.length) {
    console.log(gateFindings.map((finding) => `  [FAIL] gate:${finding.name}: ${finding.detail}`).join("\n"));
  }
  // A row that could not run on this host is not a pass; --strict makes it a blocking problem.
  for (const line of formatNotRun({ notRun })) console.error(`right-release: ${line}`);
  const total = failures.length + gateFindings.length + configProblems + (opts.strict ? notRun.length : 0);
  if (total > 0) {
    console.error(`right-release: preflight found ${total} blocking problem(s); the build would fail on these`);
    process.exit(1);
  }
  process.exit(0);
}

let prePackScanned = false; // set once a pre-pack payload scan has seen a native binary
// One checkpoint for the build: every config finding of this run and every source-gate finding are reported together, and
// nothing below (lock, install, checks, compile, signing) runs while any exists.
// Source-level gates (release-switch scan, devtools, feature closure, release dependencies, production mode) live in
// prebuild-gates.mjs so `doctor --gates` runs the exact same code in the same order. They read the expanded target (and the
// iOS spec), so a failed one of those blocks them.
const prebuildBlocker = ["target", ...(opts.platform === "ios" ? ["ios-target"] : [])].find((name) => failedChecks.has(name));
const { findings: prebuildFindings, devtoolsAllowance, inventoryText: releaseSwitchInventoryText } = prebuildBlocker
  ? { findings: [{ name: "prebuild-gates", detail: `BLOCKED by ${prebuildBlocker}` }], devtoolsAllowance: { mode: "denied" }, inventoryText: null }
  : evaluatePrebuildGates({ root, config, target, platform: opts.platform, iosSpec, nativeIos, nativeAssembly, nativeFinalizer, strict: !opts.dryRun });
if (problems.length || prebuildFindings.length) {
  printProblems();
  for (const finding of prebuildFindings) console.error(`right-release: ${finding.detail.startsWith("BLOCKED by ") ? `gate ${finding.name}: ` : ""}${finding.detail}`);
  process.exit(1);
}

const releaseLock = acquireReleaseLock(root, config.app ?? path.basename(root), opts.platform, opts.tier);
const mode = "package";
const command = target.package;
if (!command && opts.platform !== "android" && opts.platform !== "ios" && !packageCommandOptional(target, resolveProjectType({ appRoot: root, config, target }), { expanded: true })) fail(`${config.app ?? "app"} has no ${opts.platform} ${mode} command`);
if (opts.upload && !target.publish && !target.upload) {
  fail(`${config.app ?? "app"} has no ${opts.platform} upload config`);
}

const started = Date.now();
console.log(`right-release ${VERSION}: ${config.app ?? path.basename(root)} ${opts.platform} ${mode} tier=${opts.tier ?? "neutral"}`);
const macResumeAt = opts.platform === "mac" && opts.resumeFrom ? MAC_RESUME_PHASES.indexOf(opts.resumeFrom) : 0;
const runMacPhase = (phase) => opts.platform !== "mac" || !opts.resumeFrom || MAC_RESUME_PHASES.indexOf(phase) >= macResumeAt;

if (opts.install) await runInstall(config.packageManager, workdir);
await runConfiguredChecks({
  packageManager: config.packageManager,
  checks: config.checks,
  workdir,
  skipChecks: opts.skipChecks,
  run: runPackageScript,
});

let nativeFinalization = null;
let bundleMarkerReceipts = [];
if (opts.platform === "win" && target.signingContract === MSIX_SIGNING_CONTRACT) {
  // MSIX order: prePackage builds + stages the payload; buildMsix signs inner PE files (DLLs, EXEs),
  // packs the exact signed bytes, signs the package through sign-windows, verifies signer == Publisher,
  // writes the App Installer route and the seal record. Never a second signer.
  await runCommand(target.prePackage, root);
  // Raw payload scan BEFORE the package is packed (a packed MSIX is only readable through its ZIP directory).
  assertNoQaControlBinaries("msix staged payload", [resolveDeclaredPath(root, target.msix.stageDir)], { requireBinary: true, prePack: true });
  const options = msixBuildOptions({ root, target, app: config.app, dryRun: opts.dryRun, receipts: {
    pe: windowsReceiptPath(root, "raw-exe"), msix: windowsReceiptPath(root, "installer"),
    dir: path.dirname(windowsReceiptPath(root, "raw-exe")) } });
  const built = buildMsix(options, { log: (event) => console.log(`right-release: ${JSON.stringify(event)}`) });
  console.log(`right-release: msix ${built.dryRun ? "dry-run" : "built"} ${options.out}`);
  // Second layer: read the finished package through its ZIP central directory (stored and deflate entries).
  if (!built.dryRun) assertNoQaControlBinaries("msix package", [options.out], { requireBinary: true });
} else if (opts.platform === "win") {
  const rawFiles = target.sign.prePackageFiles.map((p) => resolveDeclaredPath(root, p));
  const sidecarPlan = await prepareMediaSidecar(root);
  const externalPlan = await prepareExternalBin(root);
  await runCommand(target.prePackage, root);
  for (const file of rawFiles) await mustExist(file, `missing raw EXE signing artifact: ${file}`);
  const sidecarPre = sidecarPlan && !opts.dryRun ? verifyStagedSidecars(sidecarPlan) : [];
  const externalPre = externalPlan && !opts.dryRun ? verifyExternalBins(externalPlan) : [];
  const sidecarFiles = [...sidecarPre, ...externalPre].map((file) => file.stagedPath);
  assertNoQaControlBinaries("windows media sidecar and externalBin", sidecarFiles, { prePack: true });
  // Before signing and packaging: the raw EXE is uncompressed, so a QA control server is visible here.
  assertNoQaControlBinaries("windows raw EXE", rawFiles, { requireBinary: true, prePack: true });
  // The marker must reach its FINAL value before signing. Tauri rewrites it during
  // bundling, and doing that to signed bytes invalidates Authenticode — the exact
  // defect that shipped invalid binaries. Idempotent, so an app that still patches
  // in its own prePackage script simply reports alreadyPatched here.
  const isNsis = target.signingContract === WINDOWS_NSIS_SIGNING_CONTRACT;
  bundleMarkerReceipts = !isNsis || opts.dryRun ? [] : rawFiles.map((file) => patchTauriBundleType(file, { bundle: target.bundleMarker ?? "nsis" }));
  for (const receipt of bundleMarkerReceipts) {
    console.log(`right-release: bundle marker ${receipt.bundle} ${receipt.alreadyPatched ? "already applied" : "applied"} at offset ${receipt.offset} in ${path.basename(receipt.file)}`);
  }
  await signWindows([...rawFiles, ...sidecarFiles], "raw-exe", root);
  if (sidecarPlan && !opts.dryRun) {
    const signed = Object.fromEntries(sidecarPre.map((file) => [file.tool, sidecarFileEvidence(file.stagedPath)]));
    writeMediaSidecarReceipt(root, buildMediaSidecarReceipt(sidecarPlan, sidecarPre, signed));
  }
  if (externalPlan && !opts.dryRun) {
    const signed = Object.fromEntries(externalPre.map((file) => [file.tool, sidecarFileEvidence(file.stagedPath)]));
    writeExternalBinReceipt(root, buildExternalBinReceipt(externalPlan, externalPre, signed));
  }
  if (nativeAssembly?.packageHook && !hasNativeTargetSpecificPrePackage(target, nativeAssembly.packageHook)) {
    await runCommand(nativeAssembly.packageHook, root);
  }
  if (nativeFinalizer) {
    nativeFinalization = await runNativeFinalizer({
      finalizer: nativeFinalizer,
      nativeAssembly,
      target,
      root,
      signedFiles: rawFiles,
    });
  }
} else if (opts.platform === "mac") {
  const sidecarPlan = await prepareMediaSidecar(root, { stage: runMacPhase("prepackage") });
  const externalPlan = await prepareExternalBin(root, { stage: runMacPhase("prepackage") });
  if (runMacPhase("prepackage")) await runCommand(target.prePackage, root);
  // Resolve after prePackage: with storage governance the artifact may live only in RIGHT_RELEASE_ARTIFACT_DIR.
  const signingEntries = macSigning.entries.map((entry) => ({ ...entry, file: resolveReleaseArtifactPath(root, entry.file) }));
  for (const { file } of signingEntries) await mustExist(file, `missing macOS Developer ID signing artifact: ${file}`);
  assertNoQaControlBinaries("macOS signing set", signingEntries.map(({ file }) => file), { requireBinary: true, prePack: true });
  // The DMG is built from the .app (notarize.app / the signing set); a compressed DMG cannot be scanned afterwards.
  if (target.notarize.app) {
    const stagedApp = resolveReleaseArtifactPath(root, target.notarize.app);
    await mustExist(stagedApp, `missing macOS app to be packed into the DMG: ${stagedApp}`);
    assertNoQaControlBinaries("macOS app staged for the DMG", [stagedApp], { requireBinary: true, prePack: true });
  }
  if (runMacPhase("sign")) {
    await signMacos(signingEntries, root, "signing", macSigning.nestedCode, macSigning.nestedSigning);
    if (sidecarPlan && !opts.dryRun) {
      const app = signingEntries.find((entry) => /\.app\/?$/.test(entry.file));
      if (!app) fail("mediaSidecar on macOS needs an .app entry in sign.prePackageFiles");
      writeMediaSidecarReceipt(root, finishMacSidecar(sidecarPlan, app.file, root));
    }
    if (externalPlan && !opts.dryRun) {
      const app = signingEntries.find((entry) => /\.app\/?$/.test(entry.file));
      if (!app) fail("externalBin on macOS needs an .app entry in sign.prePackageFiles");
      writeExternalBinReceipt(root, finishMacExternalBin(externalPlan, app.file, root));
    }
    // Opt-in (notarize.appBeforePackage): staple the .app itself before the package step copies it into the DMG.
    if (target.notarize.appBeforePackage) await notarizeAppBeforePackage(root);
  }
} else if (opts.platform === "android") {
  // Gradle bundleRelease with upload-key signing (secrets by file path only), then the in-process AAB scan, signer pin and receipt.
  await runAndroidRelease({
    root, app: config.app ?? path.basename(root), target: androidTarget, dryRun: opts.dryRun,
    runGradle: (gradle, env) => runCommand(gradle, root, { env }),
    resolveArtifact: (declared) => resolveReleaseArtifactPath(root, declared),
    scanFiles: (label, files, scanOptions) => assertNoQaControlBinaries(label, files, scanOptions),
    receiptDir: path.resolve(process.env.RIGHT_RELEASE_STATE_ROOT ?? path.join(root, ".right-release", "receipts")),
    allowContentOnlyDevtools: devtoolsAllowance.mode === "content-only",
  });
} else if (opts.platform === "ios") {
  // Archive and export with Xcode automatic signing in the GUI session, then the IPA verification and the receipt (macOS only;
  // --dry-run prints the plan anywhere). Upload is a separate command: `right-release upload --platform ios`.
  await runIosPackageRecipe({
    root, app: config.app ?? path.basename(root), spec: iosSpec, dryRun: opts.dryRun,
    receiptDir: path.resolve(process.env.RIGHT_RELEASE_STATE_ROOT ?? path.join(root, ".right-release", "receipts")),
    scanFiles: (label, files, scanOptions) => assertNoQaControlBinaries(label, files, scanOptions),
    // The build number counter outlives every release id: it lives in the vault state root that right-release build exports.
    ...(process.env.RIGHT_RELEASE_VAULT_STATE_ROOT ? { counterDir: path.join(path.resolve(process.env.RIGHT_RELEASE_VAULT_STATE_ROOT), "ios-build-numbers") } : {}),
  });
} else if (nativeAssembly) {
  if (nativeAssembly.packageHook) await runCommand(nativeAssembly.packageHook, root);
  if (nativeFinalizer) {
    nativeFinalization = await runNativeFinalizer({
      finalizer: nativeFinalizer,
      nativeAssembly,
      target,
      root,
      signedFiles: target.sign?.prePackageFiles ?? target.sign?.files ?? [],
    });
  }
}

// Extra files or directories the app declares (for example the unpacked bundle or a staged payload).
assertNoQaControlBinaries("target.qaControlScan", (target.qaControlScan ?? []).map((p) => resolveReleaseArtifactPath(root, p)), { requireBinary: (target.qaControlScan ?? []).length > 0, prePack: true });

// Frontend bundle: no browser-QA mock may ship. Read-only, so it also runs under --dry-run (against a dist that already exists).
assertNoWebBundleMocksInFrontend("pre-package", { required: false });

if (opts.platform === "mac" && runMacPhase("package")) {
  // SwiftPM + appBeforePackage: the .app is stapled by now, so right-release packs it into the DMG itself.
  const swiftDmg = swiftMacDmgPlan({ target, projectType: resolveProjectType({ appRoot: root, config, target }), appName: config.app ?? path.basename(root), resolve: (p) => resolveReleaseArtifactPath(root, p) });
  if (swiftDmg) {
    if (opts.dryRun) console.log(`dry-run: build DMG ${swiftDmg.out} from stapled app ${swiftDmg.app}, then sign it`);
    else await buildStapledSwiftMacDmg(swiftDmg, { remove: (file) => rmSync(file, { force: true }), signDmg: (dmg) => signMacos([{ file: dmg, options: [] }], root, "dmg-signing") });
  }
}
if (command && (opts.platform !== "mac" || runMacPhase("package"))) await runCommand(command, root);
// The package command builds the frontend; a real run must now find and clean it.
assertNoWebBundleMocksInFrontend("post-package", { required: !opts.dryRun });

for (const rel of target.artifacts ?? []) {
  await mustExist(resolveReleaseArtifactPath(root, rel), `missing release artifact: ${rel}`);
}

if (nativeFinalization) await completeNativeFinalization(nativeFinalization, { root, target, nativeAssembly });

if (opts.platform === "win" && target.signingContract === WINDOWS_NSIS_SIGNING_CONTRACT) {
  const files = target.sign.files.map((p) => resolveReleaseArtifactPath(root, p));
  for (const file of files) await mustExist(file, `missing signing artifact: ${file}`);
  // Before the installer earns its own signature, prove it carries the bytes we
  // signed. An installer-only signature on a mutated payload verifies perfectly
  // and still installs a binary Windows rejects.
  await verifyWindowsEmbedding({ root, target, files, markers: bundleMarkerReceipts });
  await signWindows(files, "installer", root);
  if (target.postSign) await runCommands(target.postSign, root);
  const updaterFiles = [...new Set((target.updater?.artifacts ?? []).map((artifact) => resolveReleaseArtifactPath(root, artifact.file)))];
  if (!updaterFiles.length) fail(`${config.app} win must declare updater.artifacts for post-code-signing minisign`);
  // The key comes from the OS credential provider inside sign-updater; targets.win.updater.keyFile is refused at config validation and never forwarded.
  await run("node", [SIGN_UPDATER, ...(opts.dryRun ? ["--dry-run"] : []), ...updaterFiles.flatMap((file) => ["--file", file])], root);
}

if (target.postSign && opts.platform !== "win" && (opts.platform !== "mac" || runMacPhase("post-sign"))) {
  await runCommands(target.postSign, root);
}

if (opts.platform === "mac") {
  const archive = resolveReleaseArtifactPath(root, target.notarize.file);
  await mustExist(archive, `missing macOS notarization archive: ${archive}`);
  await notarizeMacos(archive, root);
}

if (opts.platform !== "mac" || runMacPhase("harden")) {
  const scanTargets = target.hardening ?? target.artifacts ?? [];
  if (scanTargets.length) {
    let hardeningEvidence = null;
    if (target.hardeningAllowances?.length) {
      const receiptRoot = path.resolve(root, ".right-release", "receipts");
      mkdirSync(receiptRoot, { recursive: true });
      hardeningEvidence = path.join(receiptRoot, opts.platform + "-hardening-evidence.json");
      materializeHardeningEvidence({ root, allowances: target.hardeningAllowances, outputPath: hardeningEvidence });
    }
    // Fail closed: the installer itself (NSIS LZMA, compressed DMG) cannot be read, so the raw/staged payload must
    // have been scanned before packing. Inputs that are only ZIP containers (MSIX) are scanned in place.
    if (!opts.dryRun && !prePackScanned) fail("no pre-pack payload scan ran for this release (raw EXE, staged payload or .app); an installer cannot be scanned after packing");
    assertNoQaControlBinaries("hardening targets", scanTargets.map((p) => resolveReleaseArtifactPath(root, p)));
    await run("node", [
      HARDENING_SCAN,
      ...(hardeningEvidence ? ["--allow-evidence", hardeningEvidence] : []),
      ...scanTargets.map((p) => resolveReleaseArtifactPath(root, p)),
    ], root);
  }
}

if (opts.upload && target.publish) {
  await runCommand(target.publish, root);
}

if (opts.upload && !target.publish) {
  for (const item of target.upload ?? []) {
    const file = resolveReleaseArtifactPath(root, item.file);
    await mustExist(file, `missing upload artifact: ${item.file}`);
    await run("node", [UPLOAD_LARGE, file, item.key, item.bucket ?? "public"], root);
  }
}

if (process.env.RIGHT_RELEASE_CACHE_MODE === "shared" && process.env.RIGHT_RELEASE_CACHE_OWNER === "rightkit-v2") {
  console.log("right-release: shared cache prune delegated to RightKit Cache V2");
} else {
  sweepStaleRustArtifacts();
}

console.log(`right-release: done (${Date.now() - started}ms)`);
releaseLock.release();

function usage(code, message) {
  if (message) console.error(`right-release: ${message}\n`);
  console.error("usage: right-release build --platform mac|win [--config right-release.config.json]   (build is tier-neutral; tier applies only to: right-release upload --release <id> --platform mac|win --tier patch|update)");
  process.exit(code);
}

// Product package scripts may emit final installer bytes directly into the
// managed artifact directory. Keep legacy target-relative paths working while
// preferring managed output when the local path does not exist.
// Release and update builds never contain the QA control server (rightkit-qa-driver `qa-control`).
// The development lane (development.mjs) does not run this file and is allowed to contain it.
// `requireBinary`: an input holding no native binary (missing or empty staging dir) is a failure, not a pass.
// `prePack`: marks that the raw payload was scanned before packing. What stays unscannable after packing
// (NSIS LZMA payload, compressed DMG) is logged so the release record says so.
function assertNoQaControlBinaries(label, files, { requireBinary = false, prePack = false } = {}) {
  if (opts.dryRun || files.length === 0) return;
  try {
    const stats = assertNoQaControl(files, label, { requireBinary, allowContentOnlyDevtools: devtoolsAllowance.mode === "content-only" });
    // D4 binary layer, same inputs: no test-mode marker and no S1-S4 selector name in any native binary.
    assertNoReleaseSwitches({ binaryPaths: files, devtools: devtoolsAllowance, inventoryText: releaseSwitchInventoryText ?? undefined, appForbid: config.releaseSwitches?.forbid ?? [], requireBinary: false }, `${label} (release switches)`);
    if (prePack && stats.binaries > 0) prePackScanned = true;
    for (const opaque of stats.opaque) {
      console.log(`right-release: ${label}: ${opaque.kind.toUpperCase()} payload is compressed and cannot be scanned (${opaque.file}); the raw payload was scanned before packing`);
    }
  } catch (error) {
    fail(error.message);
  }
}

// Frontend dist directories to scan: `target.webBundleScan` (explicit, relative to the app root) or, when absent, the
// Tauri `build.frontendDist` / `build.distDir` of src-tauri/tauri.conf.json. A URL (dev server) is not a bundle.
function frontendBundleDirs() {
  if (Array.isArray(target.webBundleScan)) return target.webBundleScan.map((p) => path.resolve(root, p));
  const confPath = path.join(root, "src-tauri", "tauri.conf.json");
  if (!existsSync(confPath)) return [];
  try {
    const build = JSON.parse(readFileSync(confPath, "utf8")).build ?? {};
    const dist = build.frontendDist ?? build.distDir;
    if (typeof dist !== "string" || /^[a-z][a-z0-9+.-]*:\/\//i.test(dist)) return [];
    return [path.resolve(root, "src-tauri", dist)];
  } catch (error) { fail(`src-tauri/tauri.conf.json is unreadable, so the frontend bundle cannot be located for the mock scan: ${error.message}`); }
}

function assertNoWebBundleMocksInFrontend(phase, { required }) {
  const dirs = frontendBundleDirs() ?? [];
  const present = dirs.filter((dir) => existsSync(dir));
  if (!required && present.length === 0) return;
  if (dirs.length === 0) return;
  try {
    const { files } = assertNoWebBundleMocks(required ? dirs : present, `${phase} frontend bundle`, { requireFiles: true });
    console.log(`right-release: ${phase} frontend bundle: ${files} web file(s) scanned, no browser-QA mock marker`);
  } catch (error) { fail(error.message); }
}

// Row 7.04c (R-T2): one resolver for every declared artifact path. Checkout mode is the historical rule, byte for byte (the app root, then
// RIGHT_RELEASE_ARTIFACT_DIR); a door split maps a target-prefixed path to its bound Cargo target and never reads the checkout.
function artifactContext() {
  artifactPathContext ??= splitArtifactContext({
    cwd: root, env: process.env, nativeLayout, config, target: config.targets?.[opts.platform],
    ...(workerSource?.mode === "snapshot" ? { snapshotRoot: workerSource.snapshot } : {}),
  });
  return artifactPathContext;
}

function resolveReleaseArtifactPath(root, declared) {
  try { return resolveArtifactPath(declared, artifactContext()); }
  catch (error) { if (error instanceof ArtifactPathError) fail(error.message); throw error; }
}

/** A declared path that never fell back to the managed artifact directory (a staged payload, a raw EXE): the app root, or in a split the same resolver. */
function resolveDeclaredPath(root, declared) {
  const context = artifactContext();
  return context.mode === "split" ? resolveReleaseArtifactPath(root, declared) : path.resolve(root, declared);
}

function fail(message) {
  printProblems();
  console.error(`right-release: ${message}`);
  if (opts.doctor) console.error("right-release: doctor stopped here: this problem prevents the checks after it from running; fix it and run the doctor again to see the rest");
  process.exit(1);
}

function git(cwd, runArgs) {
  const result = spawnSync("git", runArgs, { cwd, encoding: "utf8", windowsHide: true });
  if (result.status !== 0) fail(`git ${runArgs.join(" ")} failed: ${(result.stderr ?? result.stdout ?? "").trim()}`);
  return result.stdout.trim();
}

function assertSharedSccachePrerequisite() {
  if (process.env.RIGHT_RELEASE_CACHE_MODE !== "shared") return null;
  const result = spawnSync("sccache", ["--version"], { encoding: "utf8", windowsHide: true });
  const output = `${result.stdout ?? ""}\n${result.stderr ?? ""}`;
  const version = output.match(/\b(\d+)\.(\d+)\.(\d+)\b/);
  const valid = result.status === 0 && version && isAtLeastVersion(version.slice(1).map(Number), [0, 15, 0]);
  if (!valid) fail(sccacheRefusal(version?.[0]));
  return version[0];
}

function isAtLeastVersion(actual, minimum) {
  for (let index = 0; index < minimum.length; index += 1) {
    if (actual[index] !== minimum[index]) return actual[index] > minimum[index];
  }
  return true;
}

async function mustExist(file, message) {
  if (opts.dryRun) return;
  await access(file).catch(() => fail(message));
}

async function runNativeFinalizer({ finalizer, nativeAssembly, target, root, signedFiles }) {
  if (!finalizer?.cmd) fail(`${config.app} ${opts.platform} nativeAssembly.finalizer must declare a command`);
  const signedArtifacts = nativeSignedArtifacts(signedFiles, root);
  const packageIdentity = nativePackageIdentity({ nativeAssembly, target });
  const provenance = mintNativeProvenance({
    app: config.app,
    version: config.version,
    platform: opts.platform,
    architecture: target.selectedArchitecture ?? target.architecture ?? nativeAssembly?.architecture ?? process.arch,
    targetTriple: target.cargoTarget ?? target.targetTriple ?? target.rustTarget,
    signedArtifacts,
    packageIdentity,
  });
  const outputPath = nativeFinalizerOutputPath({ finalizer, target, root });
  const finalizerCommand = {
    ...finalizer,
    args: finalizer.args.map((arg) => expandFinalizerToken(arg, { provenance, outputPath, packageIdentity })),
  };
  // A finalization receipt at outputPath from a PREVIOUS run must not
  // survive into this one: the file is preferred over the finalizer's
  // stdout below, and a stale receipt carries the prior run's provenance,
  // which fails the identity check against this run's mint. Any file
  // present after the finalizer returns was written by this run.
  if (!opts.dryRun) rmSync(outputPath, { force: true });
  const stdout = await runCommand(finalizerCommand, root, {
    captureStdout: true,
    env: {
      RIGHT_RELEASE_NATIVE_PROVENANCE: provenance,
      RIGHT_RELEASE_NATIVE_FINALIZATION_RECEIPT: outputPath,
      RIGHT_RELEASE_NATIVE_PACKAGE_IDENTITY: JSON.stringify(packageIdentity),
      RIGHT_RELEASE_NATIVE_SIGNED_ARTIFACTS: JSON.stringify(signedArtifacts),
    },
  });
  if (opts.dryRun) return { outputPath, provenance, signedArtifacts, packageIdentity, output: null };

  let output = null;
  if (existsSync(outputPath)) {
    try { output = JSON.parse(readFileSync(outputPath, "utf8")); } catch (error) { fail(`native finalizer output is not valid JSON: ${outputPath}: ${error.message}`); }
  } else if (stdout?.trim()) {
    try { output = JSON.parse(stdout); } catch { fail(`native finalizer output missing: ${outputPath}`); }
  } else {
    fail(`native finalizer output missing: ${outputPath}`);
  }
  output = enrichNativeFinalizerOutput(output, root);
  try {
    output = validateNativeFinalizationOutput(output, { expectedProvenance: provenance });
  } catch (error) {
    fail(`invalid native finalizer output: ${error.message}`);
  }
  return { outputPath, provenance, signedArtifacts, packageIdentity, output };
}

function enrichNativeFinalizerOutput(output, root) {
  if (output?.provenance) return output;
  if (output?.runtime?.provenance) return { ...output, provenance: output.runtime.provenance };
  if (!output?.output) return output;
  const manifestPath = path.join(path.resolve(root, output.output), "share", "legion", "release.json");
  if (!existsSync(manifestPath)) return output;
  try {
    const manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
    const provenance = manifest.runtime?.provenance;
    if (provenance) return { ...output, provenance };
  } catch {
    return output;
  }
  return output;
}

async function completeNativeFinalization(finalization, { root, target, nativeAssembly }) {
  if (opts.dryRun) {
    console.log(`dry-run: native finalization receipt binds archive identity at ${finalization.outputPath}`);
    return;
  }
  const archivePath = nativeArchiveCandidates({ root, target, nativeAssembly }).find((file) => existsSync(file));
  if (!archivePath) fail(`${config.app} ${opts.platform} native finalization cannot find final archive`);
  let receipt;
  try {
    receipt = createNativeFinalizationReceipt({
      app: config.app,
      version: config.version,
      platform: opts.platform,
      architecture: target.selectedArchitecture ?? target.architecture ?? nativeAssembly?.architecture ?? process.arch,
      targetTriple: target.cargoTarget ?? target.targetTriple ?? target.rustTarget,
      signedArtifacts: finalization.signedArtifacts,
      packageIdentity: finalization.packageIdentity,
      archivePath,
      finalizerOutput: finalization.output,
      root,
    });
  } catch (error) {
    fail(`native finalization archive binding failed: ${error.message}`);
  }
  mkdirSync(path.dirname(finalization.outputPath), { recursive: true });
  writeFileSync(finalization.outputPath, `${JSON.stringify(receipt, null, 2)}\n`);
  console.log(`right-release: native finalization verified ${receipt.provenance} archive=${receipt.archiveSha256}`);
}

function nativeSignedArtifacts(files, root) {
  const values = Array.isArray(files) ? files : [];
  return values.map((value) => {
    const declared = typeof value === "string" ? value : value?.path ?? value?.file;
    if (!declared) fail("native finalizer signed artifact path is missing");
    const absolute = resolveDeclaredPath(root, declared);
    // A split's bound target lies outside the app: its absolute path is the evidence there.
    const evidencePath = () => { const relative = path.relative(root, absolute).replaceAll("\\", "/"); return relative.startsWith("..") ? absolute.replaceAll("\\", "/") : relative; };
    if (opts.dryRun) return { path: evidencePath() };
    if (!existsSync(absolute)) fail(`native finalizer signed artifact missing: ${absolute}`);
    return {
      path: evidencePath(),
      sha256: createHash("sha256").update(readFileSync(absolute)).digest("hex"),
      sizeBytes: readFileSync(absolute).length,
    };
  });
}

function nativePackageIdentity({ nativeAssembly, target }) {
  const declared = nativeAssembly?.packageIdentity ?? target.packageIdentity;
  if (declared) return canonicalPackageIdentity(declared);
  const artifacts = [
    ...(target.artifacts ?? []),
    ...(target.installer?.artifacts ?? []).map((artifact) => artifact.file),
    ...(target.updater?.artifacts ?? []).map((artifact) => artifact.file),
  ].filter(Boolean);
  return canonicalPackageIdentity({
    kind: target.packageKind ?? "release",
    platform: opts.platform,
    architecture: target.selectedArchitecture ?? target.architecture ?? nativeAssembly?.architecture ?? process.arch,
    ...(artifacts.length ? { artifacts } : {}),
  });
}

function nativeArchiveCandidates({ root, target, nativeAssembly }) {
  const declared = [
    nativeAssembly?.archive,
    nativeAssembly?.archivePath,
    nativeAssembly?.packageIdentity?.path,
    target.archive,
    target.archivePath,
    ...(target.artifacts ?? []),
    ...(target.installer?.artifacts ?? []).map((artifact) => artifact.file),
    ...(target.updater?.artifacts ?? []).map((artifact) => artifact.file),
  ].filter((value) => typeof value === "string" && value.trim());
  return [...new Set(declared.map((value) => resolveDeclaredPath(root, value)))];
}

function nativeFinalizerOutputPath({ finalizer, target, root }) {
  const declared = finalizer.output
    ?? target.evidence?.provenance
    ?? target.provenance
    ?? `.right-release/receipts/native-finalization-${opts.platform}.json`;
  return path.resolve(root, declared);
}

function expandFinalizerToken(value, { provenance, outputPath, packageIdentity }) {
  const replacements = {
    "{{provenance}}": provenance,
    "{provenance}": provenance,
    "${RIGHT_RELEASE_NATIVE_PROVENANCE}": provenance,
    "$RIGHT_RELEASE_NATIVE_PROVENANCE": provenance,
    "{{receipt}}": outputPath,
    "{receipt}": outputPath,
    "${RIGHT_RELEASE_NATIVE_FINALIZATION_RECEIPT}": outputPath,
    "$RIGHT_RELEASE_NATIVE_FINALIZATION_RECEIPT": outputPath,
    "{{packageIdentity}}": JSON.stringify(packageIdentity),
    "{packageIdentity}": JSON.stringify(packageIdentity),
  };
  return Object.entries(replacements).reduce((result, [token, replacement]) => result.replaceAll(token, replacement), String(value));
}

function hasNativeTargetSpecificPrePackage(target, packageHook) {
  if (!target?.prePackage || !packageHook || target.prePackage.cmd !== packageHook.cmd) return false;
  const args = target.prePackage.args ?? [];
  return args.some((arg) => ["--platform", "--architecture", "--target", "--out"].includes(arg));
}

// Post-release target/ hygiene: `cargo sweep --installed` deletes artifacts left
// by toolchains rustup no longer has (each update orphans a multi-GB pile per
// app) and never touches anything the current toolchains produced. Runs after
// publish so it cannot race an artifact, and is best-effort: a missing
// cargo-sweep or a sweep failure must never fail a release.
function sweepStaleRustArtifacts() {
  // No Cargo host, nothing of ours to sweep.
  if (!nativeLayout) return;
  const candidates = [...new Set([root, workdir, nativeLayout.manifestDir, nativeLayout.workspaceRoot])];
  const projects = candidates.filter((dir) => existsSync(path.join(dir, "target")));
  for (const dir of projects) {
    if (opts.dryRun) {
      console.log(`dry-run: cargo sweep --installed ${dir}`);
      continue;
    }
    try {
      const result = spawnSync("cargo", ["sweep", "--installed", dir], { stdio: "inherit" });
      if (result.status !== 0) {
        console.log("right-release: stale-artifact sweep skipped (install with: cargo install cargo-sweep)");
        return;
      }
    } catch {
      console.log("right-release: stale-artifact sweep skipped (install with: cargo install cargo-sweep)");
      return;
    }
  }
}

async function runInstall(pm, cwd) {
  if (pm === "pnpm") { const t = buildLaunch("pnpm", ["install", "--frozen-lockfile"], { cwd, exists: existsSync }); await run(t.command, t.args, cwd, t.env ?? {}, { install: true }); }
  else if (pm === "npm") { const t = buildLaunch("npm", ["ci"], { cwd, exists: existsSync }); await run(t.command, t.args, cwd, t.env ?? {}, { install: true }); }
  else fail(`unsupported package manager: ${pm}`);
}

async function runPackageScript(pm, script, cwd) {
  if (script === false) return;
  // A package script (a check or a web build) goes through the one door (build-launch.mjs): a broker job on a managed host.
  const target = buildLaunch(pm, ["run", script], { cwd, exists: existsSync });
  await run(target.command, target.args, cwd, target.env ?? {});
}

// A hook may be one command object or an ordered array of them (e.g. create-mac-updater then lsclean).
function commandList(value) {
  if (value == null) return [];
  return Array.isArray(value) ? value : [value];
}

// Runs in order; run() exits the process on the first failure, so the chain fails closed.
async function runCommands(value, root, options) {
  for (const command of commandList(value)) await runCommand(command, root, options);
}

// Every configured hook (prePackage, the package command and hook, sidecar stages, gradle, the finalizer, postSign, publish) is
// arbitrary code that can build, so it goes through the one door (build-launch.mjs): this package's own right-release steps run as
// they are (their builds come back through the door), node, pnpm, cargo and rightkit-qa are broker jobs, and anything else is refused
// on a broker-managed host (rightkit#1241).
async function runCommand(command, root, { env = {}, captureStdout = false } = {}) {
  const cwd = path.resolve(root, command.cwd ?? ".");
  const commandEnvironment = { ...(await commandEnv(command, root)), ...env };
  const target = buildLaunch(command.cmd, command.args ?? [], { env: { ...process.env, ...commandEnvironment }, stepEnv: command.env, cwd, exists: existsSync });
  // A direct (Linux) launch carries PATH without RustRight shim directories (build-launch `env`).
  return run(target.command, target.args, cwd, { ...commandEnvironment, ...(target.env ?? {}) }, {
    ...command,
    captureStdout,
    timeoutMs: commandTimeoutMs(command),
  });
}

function commandTimeoutMs(command) {
  if (Object.hasOwn(command, "timeoutMs")) return command.timeoutMs;
  const tokens = [command.cmd, ...(command.args ?? [])].map((value) => String(value));
  return command.notarize === true || command.notarization === true || tokens.some((value) => /notari[sz]/i.test(value))
    ? DEFAULT_NOTARIZATION_TIMEOUT_MS
    : undefined;
}

// The workflow files of the repository, when it has any, against the hostedWorkflows policy.
async function validateHostedWorkflowFiles(root, appName, hostedWorkflows, artifacts = []) {
  const workflowDir = path.join(root, ".github", "workflows");
  if (!existsSync(workflowDir)) return;
  const workflows = (await readdir(workflowDir)).sort();
  if (workflows.length) await validateHostedWorkflows(root, appName, workflows, hostedWorkflows, artifacts);
}

function validatePackageManagerPin(pkg, appName) {
  if (pkg.packageManager !== RIGHTKIT_VERSIONS.packageManager) {
    throw new ReleaseRefusal(`${appName} packageManager must be ${RIGHTKIT_VERSIONS.packageManager}, got ${pkg.packageManager ?? "<missing>"}`, "package-manager");
  }
}

function validateNoParentWorkspaceScripts(pkg, appName) {
  const scripts = JSON.stringify(pkg.scripts ?? {});
  if (scripts.includes("../tools/right-release") || scripts.includes("../rightkit/packages/release")) {
    throw new ReleaseRefusal(`${appName} package.json must call the right-release bin, not parent-workspace release source`, "package-workspace-scripts");
  }
}

// Every @rightkit/* dependency, each violation its own line of the finding.
function validateRightKitPinSpecs(pkg, appName) {
  const found = [];
  const rightKitDeps = { ...(pkg.dependencies ?? {}), ...(pkg.devDependencies ?? {}) };
  for (const [name, specifier] of Object.entries(rightKitDeps).filter(([name]) => name.startsWith("@rightkit/"))) {
    if (FORBIDDEN_RIGHTKIT_SPEC.test(String(specifier))) {
      found.push(`${appName} ${name} must come from the published npm package, not ${specifier}`);
      continue;
    }
    const allowed = RIGHTKIT_ALLOWED.get(name);
    if (allowed && !rightKitVerifiedNpm.get(name)?.has(specifier) && !rightKitVersionAllowed(allowed, specifier)) {
      found.push(`${appName} ${name} must be one of ${[...allowed].join(", ")}, got ${specifier}`);
    }
  }
  if (found.length) throw new ReleaseRefusal(found.join("\n"), "rightkit-pin-specs", found);
}

function validateRightKitReleaseSupported(pkg, appName) {
  const releaseAllowed = RIGHTKIT_ALLOWED.get("@rightkit/release");
  const releaseSpecifier = pkg.devDependencies?.["@rightkit/release"];
  if (!releaseAllowed || !(rightKitVerifiedNpm.get("@rightkit/release")?.has(releaseSpecifier) || rightKitVersionAllowed(releaseAllowed, releaseSpecifier))) {
    throw new ReleaseRefusal(`${appName} must depend on a supported @rightkit/release version in devDependencies`, "rightkit-release-supported");
  }
}

// The top-level `on:` block(s) of a workflow: the `on` line and every indented, blank or comment line after it.
function workflowTriggerBlocks(source) {
  const lines = source.split("\n");
  const blocks = [];
  for (let index = 0; index < lines.length; index += 1) {
    if (!/^(?:on|"on"|'on')[ \t]*:/.test(lines[index])) continue;
    let end = index + 1;
    while (end < lines.length && (lines[end] === "" || /^[\s#]/.test(lines[end]))) end += 1;
    blocks.push(lines.slice(index, end).join("\n"));
  }
  return blocks;
}

// The job of a managed workflow that holds `index` ({ name, start }), or null when the offset sits outside every job.
function workflowJobAt(source, index) {
  const marker = "\njobs:\n";
  const jobsAt = source.indexOf(marker);
  if (jobsAt < 0) return null;
  const bodyStart = jobsAt + marker.length;
  const unindented = /^[^\s#]/m.exec(source.slice(bodyStart));
  const bodyEnd = unindented ? bodyStart + unindented.index : source.length;
  if (index < bodyStart || index >= bodyEnd) return null;
  let job = null;
  for (const header of source.slice(bodyStart, bodyEnd).matchAll(/^ {2}(?![\s#])([^\n]*)$/gm)) {
    const start = bodyStart + header.index;
    if (start > index) break;
    const key = /^(?:"([^"\n]*)"|'([^'\n]*)'|([^\s:"'][^\s:]*))[ \t]*:/.exec(header[1]);
    job = { name: key ? (key[1] ?? key[2] ?? key[3]) : header[1], start };
  }
  return job;
}

// Why a `secrets.<name>` reference at `index` is not the declared swift.privatePackages credential in its
// exact rendered place, or null when it is. Callers handle the signing and publish allowances first.
function swiftPrivateSecretRefusal({ source, workflow, manifest, lanes, name, index }) {
  const refuse = (reason) => `secrets.${name}: ${reason}`;
  const entries = manifest?.swift?.privatePackages;
  const declared = Array.isArray(entries)
    && entries.length > 0
    && entries.every((entry) => typeof entry?.secret === "string" && /^[A-Za-z_][A-Za-z0-9_]{0,99}$/.test(entry.secret) && !/^github_/i.test(entry.secret));
  if (!declared || !entries.some((entry) => entry.secret === name)) {
    return refuse(Array.isArray(entries) && entries.length > 0
      ? "not an allowed release secret and not declared in .rightgit.json swift.privatePackages"
      : "not an allowed release secret for this workflow");
  }
  const publishSecrets = manifest?.candidate?.releaseChain?.publishSecrets;
  const reserved = new Set([...APPLE_RELEASE_SECRETS, ...PACKAGE_MANAGER_PUBLISH_SECRETS, ...(Array.isArray(publishSecrets) ? publishSecrets : [])]);
  const collisions = entries.map((entry) => entry.secret).filter((secret) => reserved.has(secret));
  if (collisions.length > 0) {
    return refuse(`.rightgit.json swift.privatePackages declares a signing or publishing secret name (${collisions.join(", ")})`);
  }
  const jobs = SWIFT_PRIVATE_JOBS.get(workflow);
  if (!jobs) return refuse(`swift.privatePackages credentials are never rendered into ${workflow}`);
  const lane = workflow.replace(/\.yml$/, "");
  if (!lanes.includes(lane)) return refuse(`lane ${lane} is not listed in .rightgit.json`);
  if (manifest?.devArtifact?.dispatch === true) return refuse("devArtifact.dispatch is true");
  const triggerBlocks = workflowTriggerBlocks(source);
  if (triggerBlocks.length === 0) return refuse(`${workflow} has no on: block to check its triggers`);
  const untrusted = /\b(pull_request_target|workflow_run)\b/.exec(triggerBlocks.join("\n"));
  if (untrusted) return refuse(`${workflow} is triggered by ${untrusted[1]}`);
  if (!SWIFT_PRIVATE_DISPATCH_WORKFLOWS.has(workflow) && /workflow_dispatch/.test(source)) {
    return refuse(`${workflow} has a workflow_dispatch trigger`);
  }
  const job = workflowJobAt(source, index);
  if (!job) return refuse(`reference sits outside every job of ${workflow}`);
  const rule = jobs.get(job.name);
  if (!rule) return refuse(`job '${job.name}' of ${workflow} may not hold the swift.privatePackages credential (allowed: ${[...jobs.keys()].join(", ")})`);
  if (!rule.allows(manifest)) return refuse(`job '${job.name}' of ${workflow} is rendered only for ${rule.only}`);
  const lineStart = source.lastIndexOf("\n", index) + 1;
  const lineEnd = source.indexOf("\n", index);
  const line = source.slice(lineStart, lineEnd < 0 ? source.length : lineEnd);
  const envLine = /^ {10}RIGHT_GIT_PRIVATE_TOKEN_(0|[1-9]\d*): \$\{\{ secrets\.([A-Za-z0-9_]+) \}\}$/.exec(line);
  if (!envLine || envLine[2] !== name || entries[Number(envLine[1])]?.secret !== name) {
    return refuse("not a RIGHT_GIT_PRIVATE_TOKEN_<n> env line naming entry <n> of swift.privatePackages");
  }
  // The owning step is the nearest preceding step line (`      - ...`) of the same job, and the line must
  // sit in that step's own `env:` block.
  const beforeLine = source.slice(job.start, lineStart);
  const owner = [...beforeLine.matchAll(/^ {6}- [^\n]*$/gm)].at(-1);
  const ownerName = /^ {6}- name: (.+)$/.exec(owner?.[0] ?? "")?.[1];
  const sinceOwner = owner ? beforeLine.slice(owner.index).split("\n") : [];
  let header = sinceOwner.length - 1;
  while (header >= 0 && (sinceOwner[header] === "" || /^ {10}/.test(sinceOwner[header]))) header -= 1;
  if (!SWIFT_PRIVATE_STEP_NAMES.has(ownerName) || sinceOwner[header] !== "        env:") {
    return refuse('not in the env of the "Configure private Swift package access" or "Remove private Swift package access" step');
  }
  return null;
}

// @rightkit/ci `sign-existing` lane (0.2.49, Orthic-Labs/rightkit#257): signs an already-qualified artifact of another run.
// It is accepted only in its exact rendered shape: workflow_dispatch only; jobs private-repo-guard and sign-existing only;
// the default-branch gate behind the private-repo guard; the protected environment; contents: read and actions: read
// (workflow and job); exactly the three Apple Developer ID secrets, referenced inside the sign-existing job only; pinned
// allowlisted actions; and no build, notarization or release step. Returns null when the shape matches, else the reason.

function signExistingRefusal(source) {
  const jobsAt = source.indexOf("\njobs:\n");
  if (jobsAt < 0) return "it declares no jobs";
  const head = source.slice(0, jobsAt + 1);
  const jobs = source.slice(jobsAt + 7);
  const onBlock = /^on:\n((?:[ \t]+[^\n]*\n|\n)*)/m.exec(head)?.[1] ?? "";
  const triggers = [...onBlock.matchAll(/^  ([A-Za-z_]+):/gm)].map((match) => match[1]);
  if (triggers.length !== 1 || triggers[0] !== "workflow_dispatch") return "it must be triggered by workflow_dispatch only";
  if (!/^permissions:\n  contents: read\n  actions: read\n(?!  )/m.test(head)) return "workflow permissions must be exactly contents: read and actions: read";
  const names = [...jobs.matchAll(/^  ([A-Za-z0-9_-]+):[ \t]*$/gm)].map((match) => match[1]);
  if (names.join(",") !== "private-repo-guard,sign-existing") return `jobs must be exactly private-repo-guard and sign-existing, got ${names.join(", ") || "none"}`;
  const marker = "\n  sign-existing:\n";
  const guard = jobs.slice(0, jobs.indexOf(marker) + 1);
  if (!guard.startsWith(SIGN_EXISTING_GUARD) || guard.length !== SIGN_EXISTING_GUARD.length + (guard.endsWith("\n\n") ? 1 : 0)) return "the private-repo-guard job is not the managed guard";
  const sign = jobs.slice(jobs.indexOf(marker) + marker.length);
  if (!SIGN_EXISTING_JOB_HEAD.test(sign)) return "the sign-existing job must need only private-repo-guard, run only for workflow_dispatch on the default branch in an environment on a macOS runner, with job permissions exactly contents: read and actions: read";
  const secrets = [...source.matchAll(/\bsecrets\.([A-Z0-9_]+)/gi)].map((match) => match[1]);
  const signSecrets = [...sign.matchAll(/\bsecrets\.([A-Z0-9_]+)/gi)].map((match) => match[1]);
  if (secrets.length !== signSecrets.length || new Set(secrets).size !== SIGN_EXISTING_SECRETS.size || secrets.some((name) => !SIGN_EXISTING_SECRETS.has(name))) return "it must reference exactly the three Apple Developer ID secrets, inside the sign-existing job";
  for (const [, action] of sign.matchAll(/^\s*(?:- )?uses: (\S+)/gm)) {
    const [name, pin] = action.split("@");
    if (!SIGN_EXISTING_ACTIONS.has(name) || !/^[0-9a-f]{40}$/.test(pin ?? "")) return `action ${action} is not an allowed commit-pinned action`;
  }
  for (const [, command] of source.matchAll(/right-release\s+([A-Za-z][A-Za-z-]*)/g)) {
    if (!SIGN_EXISTING_RIGHT_RELEASE_COMMANDS.has(command)) return `right-release ${command} is not allowed; the lane only signs an existing artifact`;
  }
  if (!/right-release sign-dev-artifact /.test(sign)) return "it must run right-release sign-dev-artifact";
  const build = SIGN_EXISTING_BUILD_RE.exec(sign);
  if (build) return `it must not build, notarize or release (found ${JSON.stringify(build[0])})`;
  return null;
}

// The checks above are a denylist backstop. The authority is byte equality with what @rightkit/ci renders for this repository's
// manifest: an extra step (for example one that copies a secret into `env:` and posts it somewhere, or one that interpolates
// `${{ inputs.* }}` straight into `run:`) passes every pattern above but can never equal the managed render.
// The renderer comes from this monorepo's own packages/ci when present, else from the repository's installed @rightkit/ci; with
// neither the file is refused, because an unverifiable sign-existing.yml must not be accepted.
async function loadRightKitCiRenderer(root) {
  return (await loadRightKitCiModule(root, "sync.mjs", "renderLanes"))?.renderLanes ?? null;
}

async function loadRightKitCiModule(root, file, exportName) {
  const candidates = [pathToFileURL(fileURLToPath(new URL(`../ci/${file}`, import.meta.url))).href];
  try {
    candidates.push(pathToFileURL(createRequire(path.join(root, "package.json")).resolve(`@rightkit/ci/${file}`)).href);
  } catch {
    // not installed in the repository; the sibling candidate may still apply
  }
  for (const href of candidates) {
    try {
      if (href.startsWith("file:") && !existsSync(fileURLToPath(href))) continue;
      const loaded = await import(href);
      if (typeof loaded[exportName] === "function") return loaded;
    } catch {
      // try the next candidate
    }
  }
  return null;
}

async function signExistingRenderRefusal(root, manifest, source) {
  const renderLanes = await loadRightKitCiRenderer(root);
  if (!renderLanes) return "the @rightkit/ci renderer is not available (install @rightkit/ci as a devDependency), so the file cannot be compared with its managed render";
  let expected;
  try {
    expected = renderLanes(manifest, root).find((lane) => lane.filename === "sign-existing.yml")?.content;
  } catch (error) {
    return `the manifest does not render a sign-existing lane (${error.message})`;
  }
  if (typeof expected !== "string") return "the manifest does not render a sign-existing lane";
  if (source.replace(/\r\n?/g, "\n") !== expected.replace(/\r\n?/g, "\n")) return "the file is not byte-for-byte the @rightkit/ci render of this repository's .rightgit.json (run right-git sync); extra, changed or reordered steps are refused";
  return null;
}


// `workflows` block entries render from code, so they are admitted only when the block passes @rightkit/ci's own
// validateWorkflows and each file is byte-for-byte the @rightkit/ci render. With no resolvable @rightkit/ci they are refused.
async function validateDeclaredWorkflowBlock(root, appName, manifest, declaredFiles) {
  const validator = (await loadRightKitCiModule(root, "workflows.mjs", "validateWorkflows"))?.validateWorkflows;
  const renderLanes = await loadRightKitCiRenderer(root);
  if (!validator || !renderLanes) throw new ReleaseRefusal(`${appName} right-git-ci-only policy cannot verify .rightgit.json workflows: @rightkit/ci is not available (install @rightkit/ci as a devDependency)`, "hosted-workflows");
  const errors = [];
  validator(manifest, errors);
  if (errors.length > 0) throw new ReleaseRefusal(`${appName} right-git-ci-only policy refuses the .rightgit.json workflows block: ${errors.join("; ")}`, "hosted-workflows");
  let rendered;
  try {
    rendered = renderLanes(manifest, root);
  } catch (error) {
    throw new ReleaseRefusal(`${appName} right-git-ci-only policy cannot render the .rightgit.json workflows block: ${error.message}`, "hosted-workflows");
  }
  for (const file of declaredFiles) {
    const expected = rendered.find((entry) => entry.filename === file)?.content;
    const actual = await readFile(path.join(root, ".github", "workflows", file), "utf8");
    if (typeof expected !== "string" || actual.replace(/\r\n?/g, "\n") !== expected.replace(/\r\n?/g, "\n")) {
      throw new ReleaseRefusal(`${appName} ${file} is not byte-for-byte the @rightkit/ci render of its .rightgit.json workflows entry (run right-git sync)`, "hosted-workflows");
    }
  }
}

// Structural backstop for declared workflows (after render equality): the one top-level permissions block is exactly
// `contents: read`, no other permissions key exists, no secrets reference, every `uses:` is commit-pinned, and every
// checkout step sets persist-credentials: false. Steps are split on step-list indentation, not matched as free text.
function declaredWorkflowPolicyRefusal(appName, workflow, source) {
  const refuse = (why) => { throw new ReleaseRefusal(`${appName} right-git managed ${workflow} violates the declared-workflow policy: ${why}`, "hosted-workflows"); };
  const lines = source.replace(/\r\n?/g, "\n").split("\n");
  const permissionsAt = lines.flatMap((line, i) => (/^\s*permissions\s*:/.test(line) ? [i] : []));
  if (permissionsAt.length !== 1 || lines[permissionsAt[0]] !== "permissions:" || lines[permissionsAt[0] + 1] !== "  contents: read" || /^ {2,}\S/.test(lines[permissionsAt[0] + 2] ?? "")) refuse("permissions must be exactly one top-level `contents: read`");
  if (/\bsecrets\b/i.test(source)) refuse("it references the secrets context");
  let step = null;
  const steps = [];
  for (const line of lines) {
    if (/^ {6}- /.test(line)) { step = [line]; steps.push(step); }
    else if (step && (/^ {7,}\S/.test(line) || line === "")) step.push(line);
    else step = null;
  }
  for (const body of steps) {
    const uses = /^ {6}- uses: (\S+)|^ {8}uses: (\S+)/m.exec(body.join("\n"));
    if (!uses) continue;
    const ref = uses[1] ?? uses[2];
    if (!/^[A-Za-z0-9_.-]+\/[A-Za-z0-9_./-]+@[0-9a-f]{40}$/.test(ref)) refuse(`action ${ref} is not commit-pinned`);
    if (/^actions\/checkout@/.test(ref) && !body.some((line) => /^ {10}persist-credentials: false\s*$/.test(line))) refuse("a checkout step does not set persist-credentials: false");
    if (body.some((line) => /persist-credentials:\s*true/.test(line))) refuse("persist-credentials: true");
  }
  if (/^\s*(?:- )?uses:/m.test(lines.filter((l) => !steps.some((b) => b.includes(l))).join("\n"))) refuse("a uses: sits outside a step list");
}

async function validateHostedWorkflows(root, appName, workflows, policy, artifacts = []) {
  // A repository that ships more than the released app (a monorepo's web or server pipeline) declares each workflow and why;
  // none may reach a secret but GITHUB_TOKEN or run release machinery (hosted-workflow-policy.mjs).
  if (policy !== null && typeof policy === "object" && policy.policy === DECLARED_UNRELATED_POLICY) {
    const texts = new Map();
    for (const file of workflows) texts.set(file, await readFile(path.join(root, ".github", "workflows", file), "utf8"));
    const refusal = declaredUnrelatedRefusal({ declaration: policy, workflows, read: (file) => texts.get(file), artifacts });
    if (refusal) throw new ReleaseRefusal(`${appName} ${DECLARED_UNRELATED_POLICY} hosted workflows refused: ${refusal}`, "hosted-workflows");
    return;
  }
  if (policy !== "right-git-ci-only") {
    throw new ReleaseRefusal(`${appName} hosted workflow files are forbidden: ${workflows.join(", ")} (set hostedWorkflows to "right-git-ci-only", or to { policy: "${DECLARED_UNRELATED_POLICY}", files: { <file>: <reason> } } when they are a pipeline unrelated to the released app)`, "hosted-workflows");
  }
  const manifestPath = path.join(root, ".rightgit.json");
  let manifest;
  try {
    manifest = JSON.parse(await readFile(manifestPath, "utf8"));
  } catch {
    throw new ReleaseRefusal(`${appName} right-git-ci-only policy requires a valid .rightgit.json`, "hosted-workflows");
  }
  const lanes = manifest?.lanes;
  // Mirrors the `lanes` enum in @rightkit/ci manifest.schema.json.
  const safeLanes = new Set(["ci", "swift-check", "qualification", "package-smoke", "release-candidate", "publish-npm", "publish-package-managers", "regenerate", "sign-existing"]);
  if (manifest?.schemaVersion !== 1
    || manifest?.compile !== "github-actions-only"
    || !Array.isArray(lanes)
    || lanes.length === 0
    || !(lanes.includes("ci") || lanes.includes("swift-check"))
    || new Set(lanes).size !== lanes.length
    || lanes.some((lane) => !safeLanes.has(lane))) {
    throw new ReleaseRefusal(`${appName} right-git-ci-only policy requires compile=github-actions-only plus unique .rightgit.json lanes drawn from [${[...safeLanes].map((lane) => `"${lane}"`).join(", ")}] and including "ci" or "swift-check"`, "hosted-workflows");
  }
  // Workflows declared under the manifest's `workflows` block (@rightkit/ci workflows.mjs: explicit `file`,
  // else `<name>.yml`) are managed files too, in addition to the lane files.
  const declaredWorkflows = manifest?.workflows;
  if (declaredWorkflows !== undefined && (declaredWorkflows === null || typeof declaredWorkflows !== "object" || Array.isArray(declaredWorkflows))) {
    throw new ReleaseRefusal(`${appName} right-git-ci-only policy requires .rightgit.json workflows to be an object`, "hosted-workflows");
  }
  const declaredFiles = Object.entries(declaredWorkflows ?? {}).map(([name, config]) => {
    if (config === null || typeof config !== "object" || Array.isArray(config)) throw new ReleaseRefusal(`${appName} right-git-ci-only policy requires .rightgit.json workflows.${name} to be an object`, "hosted-workflows");
    const file = typeof config?.file === "string" ? config.file : `${name}.yml`;
    if (!/^[A-Za-z0-9][A-Za-z0-9._-]{0,60}\.yml$/.test(file)) throw new ReleaseRefusal(`${appName} right-git-ci-only policy refuses workflow file name ${JSON.stringify(file)} declared by .rightgit.json`, "hosted-workflows");
    return file;
  });
  const expectedWorkflows = [...new Set([...lanes.map((lane) => `${lane}.yml`), ...declaredFiles])].sort();
  if (JSON.stringify(workflows) !== JSON.stringify(expectedWorkflows)) {
    throw new ReleaseRefusal(`${appName} right-git-ci-only policy requires workflows declared by .rightgit.json; expected ${expectedWorkflows.join(", ")}, got: ${workflows.join(", ")}`, "hosted-workflows");
  }
  if (declaredFiles.length > 0) await validateDeclaredWorkflowBlock(root, appName, manifest, declaredFiles);
  for (const workflow of expectedWorkflows) {
    const source = await readFile(path.join(root, ".github", "workflows", workflow), "utf8");
    if (!source.startsWith("# Managed by right-git")) {
      throw new ReleaseRefusal(`${appName} ${workflow} is not marked as right-git managed`, "hosted-workflows");
    }
    if (declaredFiles.includes(workflow)) declaredWorkflowPolicyRefusal(appName, workflow, source);
    const jobSpan = (jobName) => {
      const marker = `\n  ${jobName}:\n`;
      const start = source.indexOf(marker);
      if (start < 0) return null;
      const remainder = source.slice(start + marker.length);
      const next = /\n  [A-Za-z0-9_-]+:\n/.exec(remainder);
      return { start, end: next ? start + marker.length + next.index : source.length };
    };
    const inside = (index, span) => span && index >= span.start && index < span.end;
    const requestedSecrets = [...source.matchAll(/\bsecrets\.([A-Z0-9_]+)/gi)].map((match) => ({ name: match[1], index: match.index }));
    const allowedMacosSecrets = new Set(APPLE_RELEASE_SECRETS);
    const windowsSpan = jobSpan("windows-sign");
    const macosSpan = jobSpan("macos-sign");
    const publishSpan = jobSpan("publish");
    const windowsSource = windowsSpan ? source.slice(windowsSpan.start, windowsSpan.end) : "";
    const macosSource = macosSpan ? source.slice(macosSpan.start, macosSpan.end) : "";
    const publishSource = publishSpan ? source.slice(publishSpan.start, publishSpan.end) : "";
    // Two release-chain shapes are accepted: the retired tag-first lane (a pushed
    // `refs/tags/v*` — or, for signing only, an explicit manual `signed_qualification`
    // dispatch — gates signing/publish) and the current tag-last lane (one admitted
    // dispatch binds every stage to an exact main commit; an `admission` job validates
    // the dispatch envelope and publication itself mints the tag last). Both must prove
    // the same thing — signing is unreachable without qualification actually being
    // requested for this run, and publish is unreachable unless that qualification
    // verified success in this same run — only the mechanism differs.
    const legacySigningGate = (job) => new RegExp(
      `${job}:\\n    needs: \\[candidate\\]\\n    if: \\$\\{\\{ startsWith\\(github\\.ref, 'refs\\/tags\\/v'\\)(?: \\|\\| \\(github\\.event_name == 'workflow_dispatch' && inputs\\.signed_qualification == true\\))? \\}\\}`,
    );
    const admittedSigningGate = (job) => new RegExp(
      // `, dev-qualification` (@rightkit/ci candidate.devQualification, #107) only adds a gate.
      `${job}:\\n    needs: \\[admission, candidate(?:, dev-qualification)?\\]\\n    if: \\$\\{\\{ needs\\.admission\\.outputs\\.signed_qualification == 'true' \\}\\}`,
    );
    const protectedWindowsSigning = workflow === "release-candidate.yml"
      && manifest?.candidate?.signWindows === true
      && (legacySigningGate("windows-sign").test(windowsSource) || admittedSigningGate("windows-sign").test(windowsSource));
    const protectedMacosSigning = workflow === "release-candidate.yml"
      && manifest?.candidate?.signMacos === true
      && (legacySigningGate("macos-sign").test(macosSource) || admittedSigningGate("macos-sign").test(macosSource));
    // @rightkit/ci `devArtifact.mac: "sign"` renders a `dev-mac-sign` job into
    // the managed ci.yml: push to the default branch only, release environment,
    // Developer ID certificate secrets only (no notarization keys).
    // @rightkit/ci `regenerate` lane: manual-only, opens a pull request from a
    // right-git/regenerate-<run> branch and never writes to the default branch.
    // `contents: write` and `pull-requests: write` are accepted only inside that
    // job, only when the manifest declares `regenerate`, and only when the
    // workflow has no trigger other than workflow_dispatch and pushes solely to
    // the right-git/regenerate-* branch namespace.
    const regenerateSpan = workflow === "regenerate.yml" ? jobSpan("regenerate") : null;
    const regenerateSource = regenerateSpan ? source.slice(regenerateSpan.start, regenerateSpan.end) : "";
    const protectedRegenerate = workflow === "regenerate.yml"
      && manifest?.regenerate && typeof manifest.regenerate.script === "string"
      && /^on:\n  workflow_dispatch:\n(?!  )/m.test(source)
      && /regenerate:\n    needs: \[private-repo-guard\]\n    if: \$\{\{ github\.event_name == 'workflow_dispatch' \}\}\n/.test(regenerateSource)
      && /branch="right-git\/regenerate-\$\{\{ github\.run_id \}\}-\$\{\{ github\.run_attempt \}\}"\n/.test(regenerateSource)
      && /git push origin "HEAD:refs\/heads\/\$branch"\n/.test(regenerateSource)
      && !/git push(?![^\n]*"HEAD:refs\/heads\/\$branch")/.test(regenerateSource);
    const allowedRegeneratePermissions = new Set(["contents", "pull-requests"]);
    // @rightkit/ci `supersede` job (one CI run per repo, #338): `actions: write` is accepted only for the
    // job whose head matches the managed shape exactly (push to the default branch only, needs only
    // private-repo-guard, contents: read plus actions: write, no action `uses:`), and only in ci.yml when
    // the manifest has not opted into `concurrency.mainPushes: "queue"` (which renders no such job).
    const supersedeSpan = workflow === "ci.yml" ? jobSpan("supersede") : null;
    const supersedeSource = supersedeSpan ? source.slice(supersedeSpan.start, supersedeSpan.end) : "";
    const protectedSupersede = workflow === "ci.yml"
      && manifest?.concurrency?.mainPushes !== "queue"
      && SUPERSEDE_JOB_HEAD.test(supersedeSource)
      && !/^\s*-?\s*uses:/m.test(supersedeSource);
    const devMacSpan = workflow === "ci.yml" ? jobSpan("dev-mac-sign") : null;
    const devMacSource = devMacSpan ? source.slice(devMacSpan.start, devMacSpan.end) : "";
    // `mac: "notarize"` additionally allows the notary API key secrets; the
    // job gate (push to the default branch, release environment) is the same.
    const allowedDevMacSecrets = manifest?.devArtifact?.mac === "notarize"
      ? allowedMacosSecrets
      : new Set(["APPLE_CERTIFICATE_BASE64", "APPLE_CERTIFICATE_PASSWORD", "APPLE_KEYCHAIN_PASSWORD"]);
    const protectedDevMacSigning = workflow === "ci.yml"
      && (manifest?.devArtifact?.mac === "sign" || manifest?.devArtifact?.mac === "notarize")
      && /dev-mac-sign:\n(?:    #[^\n]*\n)*    needs: \[ci\]\n    if: \$\{\{ github\.event_name == 'push' && github\.ref_name == github\.event\.repository\.default_branch \}\}\n    environment: /.test(devMacSource);
    // @rightkit/ci `sign-existing` lane: any deviation from the exact managed shape is refused outright.
    const signExistingSpan = workflow === "sign-existing.yml" ? jobSpan("sign-existing") : null;
    let protectedSignExisting = false;
    if (workflow === "sign-existing.yml") {
      const reason = manifest?.signExisting && typeof manifest.signExisting === "object"
        ? (signExistingRefusal(source) ?? await signExistingRenderRefusal(root, manifest, source))
        : "the manifest declares no signExisting";
      if (reason !== null) throw new ReleaseRefusal(`${appName} right-git managed sign-existing.yml is not the protected sign-existing shape: ${reason}`, "hosted-workflows");
      protectedSignExisting = true;
    }
    // @rightkit/ci `swift.privatePackages` credential: allowed only in the workflow/job pairs and on the
    // env lines listed with SWIFT_PRIVATE_JOBS; swiftPrivateSecretRefusal explains every other reference.
    const releaseChain = workflow === "release-candidate.yml" ? manifest?.candidate?.releaseChain : undefined;
    const allowedReleaseSecrets = new Set(releaseChain?.publishSecrets ?? []);
    const writePermissions = [...source.matchAll(/^\s*(actions|attestations|checks|contents|deployments|discussions|id-token|issues|models|packages|pages|pull-requests|security-events|statuses):\s*write\s*$/gmi)].map((match) => ({ name: match[1].toLowerCase(), index: match.index }));
    // Legacy: publish requires a pushed tag AND same-run installed-qualification
    // success — the tag is the sole gate because tag-first signing already proved
    // qualification before the tag existed. Tag-last: there is no tag to check;
    // instead publish requires the admission job's `publish` output (which itself
    // implies `signed_qualification` was requested — see the admission job's own
    // envelope validation) AND same-run success of BOTH installed-qualification
    // (Windows) and macos-sign (macOS), so publish is unreachable unless every
    // signing/qualification stage actually verified in this exact run.
    const legacyReleasePublishGate = /publish:\n    needs: \[[^\n]*installed-qualification[^\n]*\]\n    if: \$\{\{ startsWith\(github\.ref, 'refs\/tags\/v'\) && needs\.installed-qualification\.result == 'success'/.test(publishSource);
    const admittedReleasePublishGate = /publish:\n    needs: \[(?=[^\n]*\badmission\b)(?=[^\n]*\binstalled-qualification\b)(?=[^\n]*\bmacos-sign\b)[^\n]*\]\n    if: \$\{\{ needs\.admission\.outputs\.publish == 'true' && needs\.installed-qualification\.result == 'success' && needs\.macos-sign\.result == 'success' \}\}/.test(publishSource);
    const protectedReleasePublish = releaseChain
      && (legacyReleasePublishGate || admittedReleasePublishGate)
      && /\n    permissions:\n      contents: write\n/.test(publishSource);
    // A secret reference is refused unless a signing or publish allowance above covers it or it is the
    // declared swift.privatePackages credential in its exact rendered place.
    const refusedSecrets = requestedSecrets
      .filter(({ name, index }) => !(protectedMacosSigning && inside(index, macosSpan) && allowedMacosSecrets.has(name))
        && !(protectedDevMacSigning && inside(index, devMacSpan) && allowedDevMacSecrets.has(name))
        && !(protectedSignExisting && inside(index, signExistingSpan) && SIGN_EXISTING_SECRETS.has(name))
        && !(protectedReleasePublish && inside(index, publishSpan) && allowedReleaseSecrets.has(name)))
      .map(({ name, index }) => swiftPrivateSecretRefusal({ source, workflow, manifest, lanes, name, index }))
      .filter((reason) => reason !== null);
    if (/^\s*permissions:\s*write-all\s*$/mi.test(source)
      || writePermissions.some(({ name, index }) => {
        if (protectedRegenerate && inside(index, regenerateSpan) && allowedRegeneratePermissions.has(name)) return false;
        if (protectedSupersede && inside(index, supersedeSpan) && name === "actions") return false;
        return name === "id-token"
          ? !(protectedWindowsSigning && inside(index, windowsSpan))
          : name !== "contents" || !protectedReleasePublish || !inside(index, publishSpan);
      })
      || /pull_request_target\s*:/i.test(source)
      || /\bsecrets\s*\[/i.test(source)
      || refusedSecrets.length > 0) {
      const reasons = [...new Set(refusedSecrets)];
      const detail = reasons.length > 10 ? [...reasons.slice(0, 10), `${reasons.length - 10} more`] : reasons;
      throw new ReleaseRefusal(`${appName} right-git managed ${workflow} requests release-capable permissions or secrets${detail.length > 0 ? `: ${detail.join("; ")}` : ""}`, "hosted-workflows");
    }
  }
}

function mergeVerifiedVersions(bundled, verified) {
  const merged = new Map([...bundled].map(([name, versions]) => [name, new Set(versions)]));
  for (const [name, versions] of verified) {
    const set = merged.get(name) ?? new Set();
    for (const version of versions) set.add(version);
    merged.set(name, set);
  }
  return merged;
}

function buildAllowedVersions(published = {}, staged = {}, legacy = {}, registry = {}) {
  const allowed = new Map();
  for (const [name, value] of [
    ...Object.entries(published),
    ...Object.entries(staged),
    ...Object.entries(legacy),
    ...Object.entries(registry),
  ]) {
    const versions = allowed.get(name) ?? new Set();
    for (const version of Array.isArray(value) ? value : [value]) versions.add(version);
    allowed.set(name, versions);
  }
  return allowed;
}

async function commandEnv(command, root) {
  const env = { ...(command.env ?? {}) };
  for (const [name, rel] of Object.entries(command.envFiles ?? {})) {
    if (opts.dryRun) continue;
    env[name] = await readFile(path.resolve(root, expandEnv(rel)), "utf8");
  }
  return env;
}

function expandEnv(value) {
  return String(value).replace(/%([^%]+)%/g, (_, name) => process.env[name] ?? "");
}

// rightkit-ffmpeg decode sidecar (media-sidecar.mjs): plan from the catalog the app compiled against, write the LGPL
// notices and the source offer, optionally run the app's stage command, and (before signing) check the staged files.
async function prepareMediaSidecar(root, { stage = true } = {}) {
  if (!mediaSidecarConfig) return null;
  // Unreachable for iOS (mediaSidecar is mac and win only, refused above); a Cargo-less layout must still never be dereferenced.
  if (!nativeLayout) fail(`${config.app ?? "app"} ${opts.platform} mediaSidecar needs a native Cargo manifest to read the sidecar catalog, and this app has none`);
  let plan;
  try {
    const loaded = loadSidecarCatalog({ config: mediaSidecarConfig, root, manifestPath: nativeLayout.manifestPath });
    plan = planMediaSidecar({ config: mediaSidecarConfig, ...loaded, root });
  } catch (error) { fail(`${config.app ?? "app"} ${opts.platform} mediaSidecar: [${error.code ?? "ERROR"}] ${error.message}`); }
  console.log(`right-release: media sidecar ${plan.rows.map((row) => row.id).join(", ")}`);
  if (opts.dryRun) {
    console.log(`dry-run: would write ${plan.notices.length + 1} notice file(s) to ${mediaSidecarConfig.notices.dir} and expect ${plan.files.map((file) => path.basename(file.stagedPath)).join(", ")} in ${mediaSidecarConfig.stageDir}`);
    return plan;
  }
  if (stage) {
    try { writeMediaSidecarNotices(plan); } catch (error) { fail(`mediaSidecar: [${error.code ?? "ERROR"}] ${error.message}`); }
    if (mediaSidecarConfig.stage) await runCommand(mediaSidecarConfig.stage, root);
  }
  return plan;
}

function finishMacSidecar(plan, appPath, root) {
  let preSign;
  const signed = {};
  const checked = [];
  try { preSign = verifyStagedSidecars(plan); } catch (error) { fail(`mediaSidecar: [${error.code ?? "ERROR"}] ${error.message}`); }
  for (const file of plan.files) {
    const inBundle = path.join(appPath, ...file.bundlePath.split("/"));
    if (!existsSync(inBundle)) fail(`mediaSidecar: ${file.tool} is not inside the built app at ${file.bundlePath}; list it under Tauri externalBin`);
    signed[file.tool] = sidecarFileEvidence(inBundle);
    checked.push({ file: inBundle, sha256: signed[file.tool].sha256 });
  }
  const nestedReceipt = path.resolve(path.dirname(macosReceiptPath(root, "signing")), "macos-signing-nested.json");
  let nested;
  try { nested = JSON.parse(readFileSync(nestedReceipt, "utf8")); } catch { fail(`mediaSidecar: missing nested macOS signing receipt ${nestedReceipt}`); }
  const problems = macSidecarSigningProblems(nested, checked);
  if (problems.length) fail(`mediaSidecar: sidecar signing evidence rejected: ${problems.join("; ")}`);
  const bundled = readdirSync(appPath, { recursive: true }).map(String);
  if (!bundled.some((entry) => path.basename(entry) === MEDIA_SIDECAR_OFFER_NAME)) {
    fail(`mediaSidecar: ${MEDIA_SIDECAR_OFFER_NAME} is not inside the built app; add ${mediaSidecarConfig.notices.dir} to the Tauri bundle resources`);
  }
  return buildMediaSidecarReceipt(plan, preSign, signed);
}

// Generic Tauri externalBin sidecars (external-bin.mjs, #270): the app's own stage command writes <name>-<triple>[.exe].
async function prepareExternalBin(root, { stage = true } = {}) {
  if (!externalBinConfig) return null;
  const plan = planExternalBin({ config: externalBinConfig, root });
  console.log(`right-release: externalBin ${plan.files.map((file) => path.basename(file.stagedPath)).join(", ")}`);
  if (opts.dryRun) {
    console.log(`dry-run: would expect ${plan.files.map((file) => path.basename(file.stagedPath)).join(", ")} in ${externalBinConfig.stageDir}`);
    return plan;
  }
  if (stage && externalBinConfig.stage) await runCommand(externalBinConfig.stage, root);
  return plan;
}

function verifyExternalBins(plan) {
  try { return verifyStagedExternalBins(plan); } catch (error) { fail(`externalBin: [${error.code ?? "ERROR"}] ${error.message}`); }
}

function finishMacExternalBin(plan, appPath, root) {
  const preSign = verifyExternalBins(plan);
  const signed = {};
  const checked = [];
  for (const file of plan.files) {
    const inBundle = path.join(appPath, ...file.bundlePath.split("/"));
    if (!existsSync(inBundle)) fail(`externalBin: ${file.tool} is not inside the built app at ${file.bundlePath}; list it under Tauri externalBin`);
    signed[file.tool] = sidecarFileEvidence(inBundle);
    checked.push({ file: inBundle, sha256: signed[file.tool].sha256 });
  }
  const nestedReceipt = path.resolve(path.dirname(macosReceiptPath(root, "signing")), "macos-signing-nested.json");
  let nested;
  try { nested = JSON.parse(readFileSync(nestedReceipt, "utf8")); } catch { fail(`externalBin: missing nested macOS signing receipt ${nestedReceipt}`); }
  const problems = macSidecarSigningProblems(nested, checked);
  if (problems.length) fail(`externalBin: signing evidence rejected: ${problems.join("; ")}`);
  return buildExternalBinReceipt(plan, preSign, signed);
}

function writeExternalBinReceipt(root, receipt) {
  const file = path.resolve(process.env.RIGHT_RELEASE_STATE_ROOT ?? path.join(root, ".right-release", "receipts"), EXTERNAL_BIN_RECEIPT_NAME);
  mkdirSync(path.dirname(file), { recursive: true });
  writeFileSync(file, `${JSON.stringify(receipt, null, 2)}\n`);
  console.log(`right-release: externalBin receipt ${file}`);
}

function writeMediaSidecarReceipt(root, receipt) {
  const file = path.resolve(process.env.RIGHT_RELEASE_STATE_ROOT ?? path.join(root, ".right-release", "receipts"), MEDIA_SIDECAR_RECEIPT_NAME);
  mkdirSync(path.dirname(file), { recursive: true });
  writeFileSync(file, `${JSON.stringify(receipt, null, 2)}\n`);
  console.log(`right-release: media sidecar receipt ${file}`);
}

async function signWindows(files, phase, root) {
  const args = [SIGN_WINDOWS];
  const receipt = windowsReceiptPath(root, phase);
  if (opts.dryRun) args.push("--dry-run");
  else {
    if (existsSync(receipt)) unlinkSync(receipt);
    args.push("--receipt", receipt);
  }
  console.log(`right-release: signing windows ${phase}`);
  await run("node", [...args, ...files], root);
  if (!opts.dryRun) verifyWindowsReceipt(receipt, files, phase);
}

function windowsReceiptPath(root, phase) {
  return signingReceiptPath({ appRoot: root, target, platform: "win", phase, stateRoot: process.env.RIGHT_RELEASE_STATE_ROOT });
}

// A signing step with per-file settings (entitlements, codesign options, a stable identifier) goes to sign-macos.mjs as a manifest;
// a step with only defaults keeps the historical positional invocation.
function macSignInvocation(items, receiptPath, extra = []) {
  if (opts.dryRun || !items.some(hasNonDefaultSettings)) return [SIGN_MACOS, ...extra, "--receipt", receiptPath, ...items.map((item) => item.file)];
  const manifest = path.resolve(path.dirname(receiptPath), `${path.basename(receiptPath, ".json")}.manifest.json`);
  mkdirSync(path.dirname(manifest), { recursive: true });
  writeFileSync(manifest, `${JSON.stringify({ schema: 1, entries: items.map((item) => ({ file: item.file, ...(item.identifier ? { identifier: item.identifier } : {}), ...(item.entitlements ? { entitlements: item.entitlements } : {}), options: item.options ?? [] })) }, null, 2)}\n`);
  return [SIGN_MACOS, ...extra, "--receipt", receiptPath, "--manifest", manifest];
}

async function signMacos(entries, root, phase = "signing", nestedCode = [], nestedSigning) {
  const files = entries.map((entry) => entry.file);
  const receipt = phase === "signing" ? macosReceiptPath(root, "signing") : path.resolve(process.env.RIGHT_RELEASE_STATE_ROOT ?? path.join(root, ".right-release", "receipts"), `macos-${phase}.json`);
  // Directory entries (.app bundles): nested code first (default settings unless sign.nestedCode names it), then the bundle itself.
  let nested = [];
  try { ({ nested } = opts.dryRun ? { nested: [] } : planMacSigning({ entries, nestedCode, nestedSigning, root })); } catch (error) { fail(error.message); }
  if (opts.dryRun && (nestedCode.length || entries.some(hasNonDefaultSettings))) {
    console.log(`right-release: dry-run: per-file signing matrix (${nestedCode.length} nestedCode rule(s), ${entries.filter(hasNonDefaultSettings).length} entry setting(s)); nested code is resolved against the built bundle at sign time`);
  }
  if (nested.length) {
    const nestedReceipt = path.resolve(path.dirname(receipt), `macos-${phase}-nested.json`);
    if (existsSync(nestedReceipt)) unlinkSync(nestedReceipt);
    console.log(`right-release: signing ${nested.length} nested code item(s) inside-out`);
    await run("node", macSignInvocation(nested, nestedReceipt), root);
    verifyMacosSigningSettings(nestedReceipt, nested);
  }
  const args = macSignInvocation(entries, receipt, opts.dryRun ? ["--dry-run"] : []);
  if (!opts.dryRun && existsSync(receipt)) unlinkSync(receipt);
  console.log("right-release: signing macOS Developer ID files");
  await run("node", args, root);
  if (!opts.dryRun) { verifyMacosSigningReceipt(receipt, files); verifyMacosSigningSettings(receipt, entries); }
}

async function notarizeMacos(archive, root) {
  const receipt = macosReceiptPath(root, "notarization");
  const args = [NOTARIZE_MACOS];
  if (opts.dryRun) args.push("--dry-run");
  args.push("--receipt", receipt);
  if (target.notarize.profile) args.push("--profile", target.notarize.profile);
  if (target.notarize.staple === false) args.push("--no-staple");
  // With appBeforePackage the app already carries its own stapled ticket inside the packaged DMG.
  if (target.notarize.app && !target.notarize.appBeforePackage) args.push("--app", resolveReleaseArtifactPath(root, target.notarize.app));
  args.push(archive);
  if (!opts.dryRun && existsSync(receipt)) unlinkSync(receipt);
  console.log("right-release: notarizing macOS archive");
  await run("node", args, root);
  if (!opts.dryRun) verifyMacosNotarizationReceipt(receipt, archive);
}

async function notarizeAppBeforePackage(root) {
  const app = resolveReleaseArtifactPath(root, target.notarize.app);
  const receipt = macosReceiptPath(root, "app-notarization");
  const zip = `${app}.zip`;
  console.log("right-release: notarizing and stapling macOS app before packaging");
  if (opts.dryRun) { console.log(`dry-run: notarize app ${app} (zip ${zip}) receipt ${receipt}`); return; }
  await mustExist(app, `missing macOS app bundle for notarize.app: ${app}`);
  if (existsSync(receipt)) unlinkSync(receipt);
  try {
    notarizeAppBundle({ app, zip, receiptPath: receipt, profile: target.notarize.profile });
    validateNotarizationReceipt({ receiptPath: receipt, archive: zip });
  } catch (error) { fail(`app notarization failed: ${error.message}`); }
  finally { rmSync(zip, { force: true }); }
}

function macosReceiptPath(root, phase) {
  return signingReceiptPath({ appRoot: root, target, platform: "mac", phase, stateRoot: process.env.RIGHT_RELEASE_STATE_ROOT });
}

function verifyMacosSigningReceipt(receipt, files) {
  if (!existsSync(receipt)) fail(`missing macOS Developer ID signing receipt: ${receipt}`);
  let evidence;
  try { evidence = JSON.parse(readFileSync(receipt, "utf8")); } catch { fail(`invalid macOS Developer ID signing receipt: ${receipt}`); }
  let expectedIdentity;
  try { expectedIdentity = resolveMacosDeveloperIdIdentity(); } catch (error) { fail(error.message); }
  const expected = files.map((file) => path.resolve(file)).sort();
  const actual = Array.isArray(evidence.files) ? evidence.files.map((item) => path.resolve(item.file)).sort() : [];
  const hashes = new Map(files.map((file) => [path.resolve(file), fileEvidence(file).sha256]));
  if (evidence.schema !== 1 || evidence.signingContract !== MACOS_PORTABLE_SIGNING_CONTRACT || !expectedIdentity || evidence.identity !== expectedIdentity
    || JSON.stringify(expected) !== JSON.stringify(actual)
    || evidence.files.some((item) => item.codesign !== "strict-valid" || item.identity !== expectedIdentity || item.timestampPresent !== true || item.after?.sha256 !== hashes.get(path.resolve(item.file)))) fail(`invalid macOS Developer ID signing receipt evidence: ${receipt}`);
}

// The receipt must record the entitlements digest and codesign options the plan asked for.
function verifyMacosSigningSettings(receipt, items) {
  if (!items.some(hasNonDefaultSettings)) return;
  let evidence;
  try { evidence = JSON.parse(readFileSync(receipt, "utf8")); } catch { fail(`invalid macOS Developer ID signing receipt: ${receipt}`); }
  const problems = matrixReceiptProblems(evidence.files, items, (file) => fileEvidence(file).sha256);
  if (problems.length) fail(`macOS signing receipt does not match the signing matrix (${receipt}):\n${problems.map((line) => `- ${line}`).join("\n")}`);
}

function verifyMacosNotarizationReceipt(receipt, archive) {
  try { validateNotarizationReceipt({ receiptPath: receipt, archive }); }
  catch (error) { fail(error.message); }
}

/**
 * Close the gap between "the raw EXE was signed" and "the installer ships that
 * EXE". Three independent facts, each able to fail on its own:
 *   1. the signed EXE on disk is byte-identical to what the signing receipt says
 *      (catches an in-place rewrite during bundling),
 *   2. it still verifies as Authenticode-valid (catches a mutation that somehow
 *      preserved the hash we recorded), and
 *   3. the executable extracted back out of the installer matches it byte for
 *      byte (catches a bundler that patched its own copy and left ours alone).
 * Only the third is proof the shipped artifact is correct; the first two name
 * the cause when it is not.
 */
async function verifyWindowsEmbedding({ root, target, files, markers }) {
  if (opts.dryRun) {
    console.log("dry-run: verify installer embeds the signed raw EXE");
    return;
  }
  const rawFile = resolveDeclaredPath(root, target.sign.prePackageFiles[0]);
  const rawReceiptPath = windowsReceiptPath(root, "raw-exe");
  let rawReceipt;
  try { rawReceipt = JSON.parse(readFileSync(rawReceiptPath, "utf8")); } catch { fail(`missing raw EXE signing receipt: ${rawReceiptPath}`); }
  const signed = rawReceipt.files?.find((item) => path.resolve(item.file) === rawFile);
  const signedSha256 = signed?.after?.sha256;
  if (!signedSha256) fail(`raw EXE signing receipt does not record a signed digest for ${rawFile}`);

  await mustExist(rawFile, `signed raw EXE disappeared before packaging completed: ${rawFile}`);
  const currentSha256 = createHash("sha256").update(readFileSync(rawFile)).digest("hex");
  if (currentSha256 !== signedSha256) {
    fail(
      `packaging rewrote the signed raw EXE: signed sha256 ${signedSha256}, on-disk sha256 ${currentSha256}. `
      + "Its Authenticode signature is now invalid. Apply every deterministic byte patch before signing, never after.",
    );
  }
  // signtool, not inference: a hash match proves the bytes, this proves the chain.
  await run("node", [SIGN_WINDOWS, "--verify-only", rawFile], root);

  const embedded = [];
  for (const installer of files) {
    let receipt;
    try {
      receipt = verifyNsisEmbeddedBinary({ installer, entryName: path.basename(rawFile), expectedSha256: signedSha256 });
    } catch (error) {
      fail(error.message);
    }
    console.log(`right-release: ${path.basename(installer)} embeds the signed ${receipt.entry} (sha256 ${receipt.sha256.slice(0, 16)}…)`);
    embedded.push(receipt);
  }

  const receiptPath = windowsReceiptPath(root, "embedding");
  mkdirSync(path.dirname(receiptPath), { recursive: true });
  writeFileSync(receiptPath, `${JSON.stringify({
    schema: 1,
    rawExe: { file: rawFile, signedSha256, verifiedSha256: currentSha256 },
    bundleMarkers: markers ?? [],
    embedded,
    verifiedAt: new Date().toISOString(),
  }, null, 2)}\n`);
}

function verifyWindowsReceipt(receipt, files, phase) {
  if (!existsSync(receipt)) fail(`missing ${phase} Windows signing receipt: ${receipt}`);
  let evidence;
  try { evidence = JSON.parse(readFileSync(receipt, "utf8")); } catch { fail(`invalid ${phase} Windows signing receipt: ${receipt}`); }
  if (evidence.schema !== 1 || !Array.isArray(evidence.files) || evidence.files.length !== files.length) fail(`ambiguous ${phase} Windows signing receipt: ${receipt}`);
  const expected = files.map((file) => path.resolve(file)).sort();
  const actual = evidence.files.map((item) => path.resolve(item.file)).sort();
  const currentHashes = new Map(files.map((file) => [path.resolve(file), createHash("sha256").update(readFileSync(file)).digest("hex")]));
  if (JSON.stringify(expected) !== JSON.stringify(actual) || evidence.files.some((item) => item.authenticode !== "Valid" || item.subject !== "CN=Damned Ventures LLC" || item.timestampPresent !== true || item.after?.sha256 !== currentHashes.get(path.resolve(item.file)))) {
    fail(`invalid ${phase} Windows signing receipt evidence: ${receipt}`);
  }
}

function run(cmd, runArgs, cwd, env = {}, options = {}) {
  const printable = `${cmd} ${runArgs.join(" ")}`.trim();
  const releaseEnv = opts.tier ? { RIGHT_RELEASE_TIER: opts.tier } : {};
  if (opts.dryRun) {
    const timeout = options.timeoutMs ? ` timeout=${options.timeoutMs}ms` : "";
    const tier = opts.tier ? `RIGHT_RELEASE_TIER=${opts.tier} ` : "";
    console.log(`dry-run: (${cwd}) ${tier}${printable}${timeout}`);
    return Promise.resolve("");
  }
  const started = Date.now();
  return new Promise((resolve) => {
    const useShell = process.platform === "win32" && !/\.(?:exe|com)$/i.test(cmd);
    const child = spawn(cmd, runArgs, {
      cwd,
      // Build children resolve in production mode; package installs keep the caller's NODE_ENV (production drops devDependencies).
      env: macosCodesignEnvironment(options.install ? { ...process.env, ...releaseEnv, ...env } : productionBuildEnv({ ...process.env, ...releaseEnv, ...env })),
      stdio: options.captureStdout ? ["inherit", "pipe", "inherit"] : "inherit",
      shell: useShell,
      windowsHide: true,
    });
    let stdout = "";
    if (options.captureStdout) child.stdout?.on("data", (chunk) => { stdout += String(chunk); });
    const timer = options.timeoutMs
      ? setTimeout(() => {
          killProcessTree(child.pid);
          console.error(`right-release step timed out after ${Math.round(options.timeoutMs / 1000)}s: ${printable}`);
          process.exit(1);
        }, options.timeoutMs)
      : null;
    child.on("error", (error) => {
      if (timer) clearTimeout(timer);
      console.error(`right-release step failed to start: ${printable}`);
      console.error(error.message);
      process.exit(1);
    });
    child.on("exit", (code) => {
      if (timer) clearTimeout(timer);
      console.log(`right-release step: ${printable} (${Date.now() - started}ms)`);
      if (code !== 0) process.exit(code ?? 1);
      resolve(options.captureStdout ? stdout : "");
    });
  });
}

function acquireReleaseLock(root, app, platform, tier) {
  const lockDir = path.join(root, ".cache", "right-release");
  const lockPath = path.join(lockDir, `${platform}.lock.json`);
  mkdirSync(lockDir, { recursive: true });
  const existing = readLock(lockPath);
  if (existing?.pid && isRightReleaseProcess(existing.pid)) {
    if (opts.dryRun) {
      console.log(`dry-run: would kill previous ${app} ${platform} release pid=${existing.pid}`);
    } else {
      console.error(`right-release: killing previous ${app} ${platform} release pid=${existing.pid}`);
      killProcessTree(existing.pid);
    }
  } else if (existing?.pid) {
    console.error(`right-release: removing stale ${app} ${platform} release lock pid=${existing.pid}`);
  }
  const lock = {
    pid: process.pid,
    app,
    platform,
    tier,
    startedAt: new Date().toISOString(),
    cwd: root,
    argv: process.argv,
  };
  writeFileSync(lockPath, `${JSON.stringify(lock, null, 2)}\n`);
  let released = false;
  const release = () => {
    if (released) return;
    released = true;
    const current = readLock(lockPath);
    if (current?.pid === process.pid) {
      try {
        unlinkSync(lockPath);
      } catch {
        // already gone
      }
    }
  };
  process.once("exit", release);
  for (const signal of ["SIGINT", "SIGTERM", "SIGHUP"]) {
    process.once(signal, () => {
      release();
      process.exit(signal === "SIGINT" ? 130 : 143);
    });
  }
  return { release };
}

function readLock(lockPath) {
  if (!existsSync(lockPath)) return null;
  try {
    return JSON.parse(readFileSync(lockPath, "utf8"));
  } catch {
    return null;
  }
}

function isRightReleaseProcess(pid) {
  if (!Number.isInteger(Number(pid)) || Number(pid) <= 0) return false;
  if (!isProcessAlive(Number(pid))) return false;
  const commandLine = processCommandLine(Number(pid));
  return /(?:right-release|release\.mjs)/i.test(commandLine);
}

function isProcessAlive(pid) {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

function processCommandLine(pid) {
  if (process.platform === "win32") {
    const result = spawnSync("powershell", [
      "-NoProfile",
      "-Command",
      `$p = Get-CimInstance Win32_Process -Filter "ProcessId = ${Number(pid)}"; if ($p) { $p.CommandLine }`,
    ], { encoding: "utf8" });
    return result.stdout ?? "";
  }
  const result = spawnSync("ps", ["-p", String(pid), "-o", "command="], { encoding: "utf8" });
  return result.stdout ?? "";
}

function killProcessTree(pid) {
  terminateProcessTree(pid);
}
