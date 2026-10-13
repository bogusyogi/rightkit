#!/usr/bin/env node
import { access, readFile, readdir } from "node:fs/promises";
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, realpathSync, rmSync, unlinkSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { spawn, spawnSync } from "./background-process.mjs";
import { validateRightKitCargoContract } from "./cargo-contract.mjs";
import { assertQaBackdoorContract } from "./qa-contract.mjs";
import { assertLegalReleaseContract } from "./legal-contract.mjs";
import { assertPrimaryReleaseCheckout, dirtyBuildInputs, resolveReleaseBuildInputs } from "./release-invocation.mjs";
import { resolveNativeCargoLayout } from "./native-cargo-layout.mjs";
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
import { collectPreflight, formatPreflight, preflightFailures } from "./preflight.mjs";
import { patchTauriBundleType } from "./tauri-bundle-marker.mjs";
import { verifyNsisEmbeddedBinary } from "./nsis-payload.mjs";
import { terminateProcessTree } from "./heavy-command.mjs";
import { materializeHardeningEvidence } from "./hardening-evidence.mjs";
import { signingReceiptPath } from "./signing-receipts.mjs";
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
import { assertNoForbiddenBuildFeatures } from "./qa-build-features.mjs";
import { assertNoReleaseSwitches, commandTokens, resolveFeatureClosure } from "./release-switch-scan.mjs";
import { devtoolsEvidence, resolveDevtoolsAllowance } from "./devtools-policy.mjs";

const TOOL_ROOT = path.dirname(fileURLToPath(import.meta.url));
const HARDENING_SCAN = path.resolve(TOOL_ROOT, "hardeningscan.mjs");
const UPLOAD_LARGE = path.resolve(TOOL_ROOT, "upload-large.mjs");
const SIGN_WINDOWS = path.resolve(TOOL_ROOT, "sign-windows.mjs");
const SIGN_MACOS = path.resolve(TOOL_ROOT, "sign-macos.mjs");
const NOTARIZE_MACOS = path.resolve(TOOL_ROOT, "notarize-macos.mjs");
const SIGN_UPDATER = path.resolve(TOOL_ROOT, "sign-updater.mjs");
const RIGHTKIT_VERSIONS = JSON.parse(readFileSync(path.resolve(TOOL_ROOT, "rightkit-versions.json"), "utf8"));
const VERSION = RIGHTKIT_VERSIONS.stagedNpm?.["@rightkit/release"] ?? RIGHTKIT_VERSIONS.npm["@rightkit/release"];
const TIERS = new Set(["patch", "update"]);
const RIGHTKIT_ALLOWED = buildAllowedVersions(
  RIGHTKIT_VERSIONS.npm,
  RIGHTKIT_VERSIONS.stagedNpm,
  RIGHTKIT_VERSIONS.legacyNpm,
  RIGHTKIT_VERSIONS.registry?.npm,
);
const RIGHTKIT_CARGO_ALLOWED = buildAllowedVersions(RIGHTKIT_VERSIONS.cargo, RIGHTKIT_VERSIONS.stagedCargo, RIGHTKIT_VERSIONS.legacyCargo);
const FORBIDDEN_RIGHTKIT_SPEC = /^(?:git|file|link|workspace):|github|github\.com/i;
const WINDOWS_NSIS_SIGNING_CONTRACT = "windows-raw-exe-authenticode-before-nsis-v1";
const WINDOWS_PORTABLE_SIGNING_CONTRACT = "windows-raw-exe-authenticode-before-portable-v1";
const DEFAULT_NOTARIZATION_TIMEOUT_MS = 30 * 60 * 1000;

// Apple Developer ID and notary secrets: the only secrets the signing jobs of the managed release chain
// (macos-sign, and dev-mac-sign when devArtifact.mac is "notarize") may reference.
const APPLE_RELEASE_SECRETS = ["APPLE_CERTIFICATE_BASE64", "APPLE_CERTIFICATE_PASSWORD", "APPLE_KEYCHAIN_PASSWORD", "APPLE_API_KEY_BASE64", "APPLE_API_KEY", "APPLE_API_ISSUER"];
// Package-manager publication tokens of the managed publish-package-managers lane.
const PACKAGE_MANAGER_PUBLISH_SECRETS = ["HOMEBREW_TAP_TOKEN", "WINGET_CREATE_GITHUB_TOKEN"];

// @rightkit/git `swift.privatePackages` (0.2.41) renders one configure step and one cleanup step, with the
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
  ["qualification.yml", new Map([["qualification", SWIFT_PRIVATE_ANY_PROFILE]])],
  ["release-candidate.yml", new Map([["candidate", SWIFT_PRIVATE_ANY_PROFILE]])],
  ["regenerate.yml", new Map([["regenerate", SWIFT_PRIVATE_ANY_PROFILE]])],
]);
// release-candidate.yml and regenerate.yml are dispatch lanes by design. In ci.yml and qualification.yml a
// workflow_dispatch trigger would let a manual run of any ref reach the credential.
const SWIFT_PRIVATE_DISPATCH_WORKFLOWS = new Set(["release-candidate.yml", "regenerate.yml"]);

const args = process.argv.slice(2);
const opts = {
  config: "right-release.config.mjs",
  platform: process.platform === "win32" ? "win" : process.platform === "darwin" ? "mac" : process.platform,
  upload: false,
  install: false,
  dryRun: false,
  skipChecks: false,
  doctor: false,
  tier: null,
  resume: false,
  resumeFrom: null,
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
const config = (await import(pathToFileURL(configPath))).default;
if (!config) usage(2, `config did not export default: ${configPath}`);
if (config.schema !== 1) fail(`unsupported config schema: ${config.schema ?? "<missing>"} (expected 1)`);

const root = path.dirname(configPath);
const workdir = path.resolve(root, config.workdir ?? ".");
const nativeLayout = resolveNativeCargoLayout({ appRoot: root, config });
const nativeAssembly = config.nativeAssembly;
const nativeFinalizer = resolveNativeFinalizerConfig(nativeAssembly);
// A SwiftPM app without a package.json has no Node package contract to validate.
if (!(resolveProjectType({ appRoot: root, config, target: config.targets?.[opts.platform] }) === "swiftpm" && !existsSync(path.join(root, "package.json")))) {
  await validateRightKitPackageContract(root, config.app, config.hostedWorkflows);
}
validateRightKitCargoContract(root, RIGHTKIT_CARGO_ALLOWED, config.app ?? path.basename(root));
const target = config.targets?.[opts.platform];
if (!target) fail(`${config.app ?? "app"} has no ${opts.platform} release target`);
const legalContract = config.legal
  ? assertLegalReleaseContract(root, config.legal, config.app ?? path.basename(root), opts.platform)
  : null;
if (target.signed !== true) {
  fail(`${config.app ?? "app"} ${opts.platform} must declare signed: true; unsigned release targets are forbidden`);
}
let macSigning; // validated macOS sign.prePackageFiles / sign.nestedCode (mac only)
if (opts.platform === "win") {
  const isNsis = target.signingContract === WINDOWS_NSIS_SIGNING_CONTRACT;
  const isPortable = target.signingContract === WINDOWS_PORTABLE_SIGNING_CONTRACT;
  const isMsix = target.signingContract === MSIX_SIGNING_CONTRACT;
  if (!isNsis && !isPortable && !isMsix) fail(`${config.app ?? "app"} win must declare signingContract: ${WINDOWS_NSIS_SIGNING_CONTRACT}, ${WINDOWS_PORTABLE_SIGNING_CONTRACT} or ${MSIX_SIGNING_CONTRACT}`);
  if (isMsix) {
    try { assertValidMsixTarget(target, config.app ?? "app"); } catch (error) { fail(error.message); }
  } else {
  if (!target.prePackage?.cmd) fail(`${config.app ?? "app"} win must declare prePackage for the raw EXE build`);
  if (!target.sign?.prePackageFiles?.length) fail(`${config.app ?? "app"} win must declare at least one sign.prePackageFiles raw EXE`);
  if (target.sign.prePackageFiles.some((file) => typeof file !== "string") || target.sign.nestedCode !== undefined) fail(`${config.app ?? "app"} win sign.prePackageFiles entries must be path strings; object entries and sign.nestedCode are macOS-only`);
  if (isNsis && target.sign.prePackageFiles.length !== 1) fail(`${config.app ?? "app"} win NSIS must declare exactly one sign.prePackageFiles raw EXE`);
  if (isNsis && !target.sign?.files?.length) fail(`${config.app ?? "app"} win NSIS must declare sign.files for installer signing`);
  if (isNsis && target.sign.prePackageFiles.some((file) => target.sign.files.includes(file))) fail(`${config.app ?? "app"} win raw EXE and installer signing files must be distinct`);
  if (isPortable && target.sign?.files?.length) fail(`${config.app ?? "app"} win portable release must sign every executable through sign.prePackageFiles and omit sign.files`);
  }
}
if (opts.platform === "mac") {
  if (target.signingContract !== MACOS_PORTABLE_SIGNING_CONTRACT) fail(`${config.app ?? "app"} mac must declare signingContract: ${MACOS_PORTABLE_SIGNING_CONTRACT}`);
  if (!target.prePackage?.cmd) fail(`${config.app ?? "app"} mac must declare prePackage to materialize Developer ID signing files`);
  if (!target.sign?.prePackageFiles?.length) fail(`${config.app ?? "app"} mac must declare at least one sign.prePackageFiles`);
  try { macSigning = validateMacSigningConfig(target.sign, { root }); } catch (error) { fail(`${config.app ?? "app"} mac ${error.message}`); }
  if (!target.notarize?.file) fail(`${config.app ?? "app"} mac must declare notarize.file`);
  if (target.notarize.appBeforePackage && !target.notarize.app) fail(`${config.app ?? "app"} mac notarize.appBeforePackage requires notarize.app`);
  if (target.notarize.staple === false && (target.notarize.app || /\.(dmg|pkg)$/i.test(target.notarize.file))) {
    fail(`${config.app ?? "app"} mac notarize.staple: false applies only to an archive of bare executables (no notarize.app, not a .dmg/.pkg)`);
  }
  const swiftDmgError = swiftMacDmgConfigError({ target, projectType: resolveProjectType({ appRoot: root, config, target }), appName: config.app ?? "app" });
  if (swiftDmgError) fail(swiftDmgError);
  if (target.dmg && typeof target.dmg === "object" && target.dmg.spec !== undefined) {
    try {
      if (typeof target.dmg.spec !== "string" || !target.dmg.spec) throw new Error("dmg.spec must be a path string");
      const specOptions = readDmgSpec(path.resolve(root, target.dmg.spec), { app: target.notarize.app });
      for (const asset of [specOptions.background, specOptions.volumeIcon]) if (asset && !existsSync(asset)) throw new Error(`referenced file not found: ${asset}`);
    } catch (error) { fail(`${config.app ?? "app"} mac targets.mac.dmg.spec: ${error.message}`); }
  }
}
if (opts.upload && target.publishBlocked) fail(`${config.app ?? "app"} ${opts.platform} publish blocked: ${target.publishBlocked}`);
/**
 * Assemble preflight inputs from the app's own files. Kept here (not in
 * preflight.mjs) so the check module stays free of release.mjs's config shape.
 */
function releasePreflight({ config, configPath, root, repoRoot, platform, nativeLayout: suppliedNativeLayout = nativeLayout, projectTarget = config.targets?.[platform] }) {
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
    ...(config.cargoLocks ?? []).map((entry) => path.resolve(root, entry)),
  ].filter((entry, index, entries) => entries.indexOf(entry) === index);
  return collectPreflight({
    platform,
    appRoot: root,
    repoRoot,
    app: config.app,
    version,
    configPath,
    cargoLockPaths,
    nativeLayout: suppliedNativeLayout,
    targetLink: suppliedNativeLayout?.targetLink,
    projectType: resolveProjectType({ appRoot: root, config, target: projectTarget }),
    msix: targetUsesMsix(projectTarget),
  });
}

if (opts.doctor) {
  const { buildInputs, nativeLayout: resolvedNativeLayout } = resolveReleaseBuildInputs({
    repoRoot: doctorInvocation.repoRoot,
    appRoot: root,
    configPath,
    config,
    target,
  });
  const dirtyInputs = dirtyBuildInputs(doctorInvocation.repoRoot, buildInputs, doctorInvocation.commit);
  if (dirtyInputs.length > 0) {
    fail(`dirty files can change the packaged app; resolve them before release:\n${dirtyInputs.map((file) => `- ${file}`).join("\n")}`);
  }
  console.log(`right-release ${VERSION}`);
  console.log(`config: ${configPath}`);
  console.log(`app: ${config.app}`);
  console.log(`platform: ${opts.platform}`);
  console.log(`tier: ${opts.tier ?? "<required for release/publish>"}`);
  console.log(`packageManager: ${config.packageManager}`);
  console.log(`workdir: ${workdir}`);
  console.log(`hardeningscan: ${HARDENING_SCAN}`);
  if (sccacheVersion) console.log(`sccache: ${sccacheVersion}`);
  if (legalContract) {
    console.log(`legal: ${legalContract.manifestPath}`);
    console.log(`legalAcceptance: ${legalContract.acceptanceVersion}`);
    console.log(`legalManifestSha256: ${legalContract.manifestSha256}`);
  }
  if (target.sign?.files?.length) console.log(`sign: ${target.sign.files.join(", ")}`);
  if (target.publishBlocked) console.log(`publishBlocked: ${target.publishBlocked}`);
  const managedRoot = process.env.RIGHTKIT_MANAGED_ROOT;
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
    fail(`artifact-storage found ${storageFindings.length} repository storage finding(s); move outputs to RIGHT_RELEASE_ARTIFACT_DIR`);
  } else console.log("artifact-storage: no installer-copy scripts detected");

  // Config above, MACHINE below. Printing config never told anyone whether this
  // box could finish a build; these checks do.
  const checks = releasePreflight({ config, configPath, root, repoRoot: doctorInvocation.repoRoot, platform: opts.platform, nativeLayout: resolvedNativeLayout });
  console.log("preflight:");
  console.log(formatPreflight(checks));
  const failures = preflightFailures(checks);
  if (failures.length > 0) {
    fail(`preflight found ${failures.length} blocking problem(s); the build would fail on these`);
  }
  process.exit(0);
}

let prePackScanned = false; // set once a pre-pack payload scan has seen a native binary
// Source-level layer of "no test-automation surface ships": refuse a release or update build whose build command
// or app Cargo manifest enables devtools, qa-native, qa-control or dev-native (also in --dry-run).
// DevTools: refused for every app except an allow-listed one that declares `devtools: "content-only"` (devtools-policy.mjs).
let devtoolsAllowance = { mode: "denied" };
try {
  devtoolsAllowance = resolveDevtoolsAllowance({ config, root });
  if (devtoolsAllowance.mode === "content-only") console.log(`right-release: devtools allowance recorded: ${JSON.stringify(devtoolsEvidence(devtoolsAllowance))}`);
  assertNoForbiddenBuildFeatures({ root, tier: "release/update", devtools: devtoolsAllowance, cargoManifest: target.cargoManifest, manifest: config.releaseSwitches?.manifest, commands: [
    { label: "target.prePackage", command: target.prePackage },
    { label: "target.package", command: target.package },
    { label: "nativeAssembly.packageHook", command: nativeAssembly?.packageHook },
    { label: "nativeAssembly.finalizer", command: nativeFinalizer },
    { label: "target.postSign", command: target.postSign },
  ] });
} catch (error) { fail(error.message); }

// D4 (M21/M23) source layer: every environment/argument read in app source is registered in the app's switches.json and
// every S1-S4 selector is compiled out of release. The inventory is `config.releaseSwitches.inventory` (relative to the
// app root) or `switches.json`; a release without one fails (a dry run only checks one that exists).
// The resolved feature graph (`cargo tree -e features`) is the AUTHORITATIVE test-mode check: it runs here, in the app
// repository, for the release target, and a release that cannot run it fails (a dry run reports and continues).
// The binary marker/name scan below is secondary evidence.
const releaseSwitchInventoryPath = path.resolve(root, config.releaseSwitches?.inventory ?? "switches.json");
const releaseSwitchInventoryText = existsSync(releaseSwitchInventoryPath) ? readFileSync(releaseSwitchInventoryPath, "utf8") : null;
try {
  let treeText;
  try {
    const closure = resolveFeatureClosure({
      root,
      manifest: config.releaseSwitches?.manifest,
      cargoManifest: target.cargoManifest,
      rust: config.releaseSwitches?.rust,
      features: config.releaseSwitches?.features ?? [],
      triple: target.cargoTarget ?? target.targetTriple ?? target.rustTarget,
      commandTexts: [target.prePackage, target.package, nativeAssembly?.packageHook, nativeFinalizer, target.postSign],
    });
    if (closure.skipped) console.log(`right-release: feature closure skipped (${closure.skipped})`);
    else treeText = closure.treeText;
  } catch (error) {
    if (!opts.dryRun) throw error;
    console.log(`right-release: dry run: ${error.message}`);
  }
  // remoteHost: rightkit-service `remote` is refused in the closure unless the app declares releaseSwitches.remoteHost: true.
  const closureInput = { treeText, remoteHost: config.releaseSwitches?.remoteHost, commands: [target.prePackage, target.package, nativeAssembly?.packageHook, nativeFinalizer, target.postSign].filter(Boolean).map((c) => commandTokens(c)) };
  if (releaseSwitchInventoryText != null) {
    assertNoReleaseSwitches({ ...closureInput, sourceRoot: root, inventoryText: releaseSwitchInventoryText, appForbid: config.releaseSwitches?.forbid ?? [] }, "source switch inventory");
  } else if (treeText != null || closureInput.commands.length) {
    assertNoReleaseSwitches({ ...closureInput, appForbid: config.releaseSwitches?.forbid ?? [] }, "feature closure");
  }
  if (releaseSwitchInventoryText == null && !opts.dryRun) {
    throw new Error(`source switch inventory: ${path.relative(root, releaseSwitchInventoryPath) || releaseSwitchInventoryPath} is missing; a release build needs the app's switches.json (see packages/release/README.md, "switches.json")`);
  }
} catch (error) { fail(error.message); }

const releaseLock = acquireReleaseLock(root, config.app ?? path.basename(root), opts.platform, opts.tier);
const mode = "package";
const command = target.package;
if (!command && !packageCommandOptional(target, resolveProjectType({ appRoot: root, config, target }))) fail(`${config.app ?? "app"} has no ${opts.platform} ${mode} command`);
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
  assertNoQaControlBinaries("msix staged payload", [path.resolve(root, target.msix.stageDir)], { requireBinary: true, prePack: true });
  const options = msixBuildOptions({ root, target, app: config.app, dryRun: opts.dryRun, receipts: {
    pe: windowsReceiptPath(root, "raw-exe"), msix: windowsReceiptPath(root, "installer"),
    dir: path.dirname(windowsReceiptPath(root, "raw-exe")) } });
  const built = buildMsix(options, { log: (event) => console.log(`right-release: ${JSON.stringify(event)}`) });
  console.log(`right-release: msix ${built.dryRun ? "dry-run" : "built"} ${options.out}`);
  // Second layer: read the finished package through its ZIP central directory (stored and deflate entries).
  if (!built.dryRun) assertNoQaControlBinaries("msix package", [options.out], { requireBinary: true });
} else if (opts.platform === "win") {
  const rawFiles = target.sign.prePackageFiles.map((p) => path.resolve(root, p));
  await runCommand(target.prePackage, root);
  for (const file of rawFiles) await mustExist(file, `missing raw EXE signing artifact: ${file}`);
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
  await signWindows(rawFiles, "raw-exe", root);
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
    await signMacos(signingEntries, root, "signing", macSigning.nestedCode);
    // Opt-in (notarize.appBeforePackage): staple the .app itself before the package step copies it into the DMG.
    if (target.notarize.appBeforePackage) await notarizeAppBeforePackage(root);
  }
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
  if (target.postSign) await runCommand(target.postSign, root);
  const updaterFiles = [...new Set((target.updater?.artifacts ?? []).map((artifact) => resolveReleaseArtifactPath(root, artifact.file)))];
  if (!updaterFiles.length) fail(`${config.app} win must declare updater.artifacts for post-code-signing minisign`);
  await run("node", [SIGN_UPDATER, ...(opts.dryRun ? ["--dry-run"] : []), ...(target.updater?.keyFile ? ["--key-file", String(target.updater.keyFile)] : []), ...updaterFiles.flatMap((file) => ["--file", file])], root);
}

if (target.postSign && opts.platform !== "win" && (opts.platform !== "mac" || runMacPhase("post-sign"))) {
  await runCommand(target.postSign, root);
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
  console.error("usage: right-release build --platform mac|win [--config right-release.config.mjs]   (build is tier-neutral; tier applies only to: right-release upload --release <id> --platform mac|win --tier patch|update)");
  process.exit(code);
}

// Product package scripts may emit final installer bytes directly into the
// managed artifact directory. Keep legacy target-relative paths working while
// preferring managed output when the local path does not exist.
// Release and update builds never contain the QA control server (rightkit-control `qa-control`).
// The development lane (development.mjs) does not run this file and is allowed to contain it.
// `requireBinary`: an input holding no native binary (missing or empty staging dir) is a failure, not a pass.
// `prePack`: marks that the raw payload was scanned before packing. What stays unscannable after packing
// (NSIS LZMA payload, compressed DMG) is logged so the release record says so.
function assertNoQaControlBinaries(label, files, { requireBinary = false, prePack = false } = {}) {
  if (opts.dryRun || files.length === 0) return;
  try {
    const stats = assertNoQaControl(files, label, { requireBinary, allowContentOnlyDevtools: devtoolsAllowance.mode === "content-only" });
    // D4 binary layer, same inputs: no test-mode marker and no S1-S4 selector name in any native binary.
    assertNoReleaseSwitches({ binaryPaths: files, inventoryText: releaseSwitchInventoryText ?? undefined, appForbid: config.releaseSwitches?.forbid ?? [], requireBinary: false }, `${label} (release switches)`);
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

function resolveReleaseArtifactPath(root, declared) {
  const local = path.resolve(root, declared);
  if (existsSync(local)) return local;
  const managed = process.env.RIGHT_RELEASE_ARTIFACT_DIR;
  if (managed) {
    const candidate = path.join(path.resolve(managed), path.basename(declared));
    if (existsSync(candidate)) return candidate;
  }
  return local;
}

function fail(message) {
  console.error(`right-release: ${message}`);
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
  if (!valid) {
    const found = version ? ` (found ${version[0]})` : "";
    fail(`shared cache mode requires sccache >= 0.15.0${found}. Install with: cargo install sccache --locked; then verify: sccache --version`);
  }
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
    const absolute = path.resolve(root, declared);
    if (opts.dryRun) return { path: path.relative(root, absolute).replaceAll("\\", "/") };
    if (!existsSync(absolute)) fail(`native finalizer signed artifact missing: ${absolute}`);
    return {
      path: path.relative(root, absolute).replaceAll("\\", "/"),
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
  return [...new Set(declared.map((value) => path.resolve(root, value)))];
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
  if (pm === "pnpm") await run("pnpm", ["install", "--frozen-lockfile"], cwd);
  else if (pm === "npm") await run("npm", ["ci"], cwd);
  else fail(`unsupported package manager: ${pm}`);
}

async function runPackageScript(pm, script, cwd) {
  if (script === false) return;
  await run(pm, ["run", script], cwd);
}

async function runCommand(command, root, { env = {}, captureStdout = false } = {}) {
  const cwd = path.resolve(root, command.cwd ?? ".");
  return run(command.cmd, command.args ?? [], cwd, { ...(await commandEnv(command, root)), ...env }, {
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

async function validateRightKitPackageContract(root, appName, hostedWorkflows) {
  const packageJsonPath = path.join(root, "package.json");
  const pkg = JSON.parse(await readFile(packageJsonPath, "utf8"));
  const scripts = pkg.scripts ?? {};
  assertQaBackdoorContract(root, scripts);
  const workflowDir = path.join(root, ".github", "workflows");
  if (existsSync(workflowDir)) {
    const workflows = (await readdir(workflowDir)).sort();
    if (workflows.length) {
      await validateHostedWorkflows(root, appName ?? pkg.name ?? "app", workflows, hostedWorkflows);
    }
  }
  if (pkg.packageManager !== RIGHTKIT_VERSIONS.packageManager) {
    fail(`${appName ?? pkg.name ?? "app"} packageManager must be ${RIGHTKIT_VERSIONS.packageManager}, got ${pkg.packageManager ?? "<missing>"}`);
  }
  if (JSON.stringify(scripts).includes("../tools/right-release") || JSON.stringify(scripts).includes("../rightkit/packages/release")) {
    fail(`${appName ?? pkg.name ?? "app"} package.json must call the right-release bin, not parent-workspace release source`);
  }
  const rightKitDeps = { ...(pkg.dependencies ?? {}), ...(pkg.devDependencies ?? {}) };
  for (const [name, specifier] of Object.entries(rightKitDeps).filter(([name]) => name.startsWith("@rightkit/"))) {
    if (FORBIDDEN_RIGHTKIT_SPEC.test(String(specifier))) {
      fail(`${appName ?? pkg.name ?? "app"} ${name} must come from the published npm package, not ${specifier}`);
    }
    const allowed = RIGHTKIT_ALLOWED.get(name);
    if (allowed && !rightKitVersionAllowed(allowed, specifier)) {
      fail(`${appName ?? pkg.name ?? "app"} ${name} must be one of ${[...allowed].join(", ")}, got ${specifier}`);
    }
  }
  const releaseAllowed = RIGHTKIT_ALLOWED.get("@rightkit/release");
  if (!releaseAllowed || !rightKitVersionAllowed(releaseAllowed, pkg.devDependencies?.["@rightkit/release"])) {
    fail(`${appName ?? pkg.name ?? "app"} must depend on a supported @rightkit/release version in devDependencies`);
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

async function validateHostedWorkflows(root, appName, workflows, policy) {
  if (policy !== "right-git-ci-only") {
    fail(`${appName} hosted workflow files are forbidden: ${workflows.join(", ")}`);
  }
  const manifestPath = path.join(root, ".rightgit.json");
  let manifest;
  try {
    manifest = JSON.parse(await readFile(manifestPath, "utf8"));
  } catch {
    fail(`${appName} right-git-ci-only policy requires a valid .rightgit.json`);
  }
  const lanes = manifest?.lanes;
  // Mirrors the `lanes` enum in @rightkit/git manifest.schema.json.
  const safeLanes = new Set(["ci", "qualification", "package-smoke", "release-candidate", "publish-npm", "publish-package-managers", "regenerate"]);
  if (manifest?.schemaVersion !== 1
    || manifest?.compile !== "github-actions-only"
    || !Array.isArray(lanes)
    || lanes.length === 0
    || !lanes.includes("ci")
    || new Set(lanes).size !== lanes.length
    || lanes.some((lane) => !safeLanes.has(lane))) {
    fail(`${appName} right-git-ci-only policy requires compile=github-actions-only plus unique .rightgit.json lanes drawn from [${[...safeLanes].map((lane) => `"${lane}"`).join(", ")}] and including "ci"`);
  }
  const expectedWorkflows = lanes.map((lane) => `${lane}.yml`).sort();
  if (JSON.stringify(workflows) !== JSON.stringify(expectedWorkflows)) {
    fail(`${appName} right-git-ci-only policy requires workflows declared by .rightgit.json; expected ${expectedWorkflows.join(", ")}, got: ${workflows.join(", ")}`);
  }
  for (const workflow of expectedWorkflows) {
    const source = await readFile(path.join(root, ".github", "workflows", workflow), "utf8");
    if (!source.startsWith("# Managed by right-git")) {
      fail(`${appName} ${workflow} is not marked as right-git managed`);
    }
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
      `${job}:\\n    needs: \\[admission, candidate\\]\\n    if: \\$\\{\\{ needs\\.admission\\.outputs\\.signed_qualification == 'true' \\}\\}`,
    );
    const protectedWindowsSigning = workflow === "release-candidate.yml"
      && manifest?.candidate?.signWindows === true
      && (legacySigningGate("windows-sign").test(windowsSource) || admittedSigningGate("windows-sign").test(windowsSource));
    const protectedMacosSigning = workflow === "release-candidate.yml"
      && manifest?.candidate?.signMacos === true
      && (legacySigningGate("macos-sign").test(macosSource) || admittedSigningGate("macos-sign").test(macosSource));
    // @rightkit/git `devArtifact.mac: "sign"` renders a `dev-mac-sign` job into
    // the managed ci.yml: push to the default branch only, release environment,
    // Developer ID certificate secrets only (no notarization keys).
    // @rightkit/git `regenerate` lane: manual-only, opens a pull request from a
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
    // @rightkit/git `swift.privatePackages` credential: allowed only in the workflow/job pairs and on the
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
        && !(protectedReleasePublish && inside(index, publishSpan) && allowedReleaseSecrets.has(name)))
      .map(({ name, index }) => swiftPrivateSecretRefusal({ source, workflow, manifest, lanes, name, index }))
      .filter((reason) => reason !== null);
    if (/^\s*permissions:\s*write-all\s*$/mi.test(source)
      || writePermissions.some(({ name, index }) => {
        if (protectedRegenerate && inside(index, regenerateSpan) && allowedRegeneratePermissions.has(name)) return false;
        return name === "id-token"
          ? !(protectedWindowsSigning && inside(index, windowsSpan))
          : name !== "contents" || !protectedReleasePublish || !inside(index, publishSpan);
      })
      || /pull_request_target\s*:/i.test(source)
      || /\bsecrets\s*\[/i.test(source)
      || refusedSecrets.length > 0) {
      const reasons = [...new Set(refusedSecrets)];
      const detail = reasons.length > 10 ? [...reasons.slice(0, 10), `${reasons.length - 10} more`] : reasons;
      fail(`${appName} right-git managed ${workflow} requests release-capable permissions or secrets${detail.length > 0 ? `: ${detail.join("; ")}` : ""}`);
    }
  }
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

// A signing step with per-file settings (entitlements, codesign options) goes to sign-macos.mjs as a manifest;
// a step with only defaults keeps the historical positional invocation.
function macSignInvocation(items, receiptPath, extra = []) {
  if (opts.dryRun || !items.some(hasNonDefaultSettings)) return [SIGN_MACOS, ...extra, "--receipt", receiptPath, ...items.map((item) => item.file)];
  const manifest = path.resolve(path.dirname(receiptPath), `${path.basename(receiptPath, ".json")}.manifest.json`);
  mkdirSync(path.dirname(manifest), { recursive: true });
  writeFileSync(manifest, `${JSON.stringify({ schema: 1, entries: items.map((item) => ({ file: item.file, ...(item.entitlements ? { entitlements: item.entitlements } : {}), options: item.options ?? [] })) }, null, 2)}\n`);
  return [SIGN_MACOS, ...extra, "--receipt", receiptPath, "--manifest", manifest];
}

async function signMacos(entries, root, phase = "signing", nestedCode = []) {
  const files = entries.map((entry) => entry.file);
  const receipt = phase === "signing" ? macosReceiptPath(root, "signing") : path.resolve(process.env.RIGHT_RELEASE_STATE_ROOT ?? path.join(root, ".right-release", "receipts"), `macos-${phase}.json`);
  // Directory entries (.app bundles): nested code first (default settings unless sign.nestedCode names it), then the bundle itself.
  let nested = [];
  try { ({ nested } = opts.dryRun ? { nested: [] } : planMacSigning({ entries, nestedCode })); } catch (error) { fail(error.message); }
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
  const rawFile = path.resolve(root, target.sign.prePackageFiles[0]);
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
      env: macosCodesignEnvironment({ ...process.env, ...releaseEnv, ...env }),
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
