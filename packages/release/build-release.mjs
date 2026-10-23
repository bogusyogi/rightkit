#!/usr/bin/env node
import { createHash } from "node:crypto";
import {
  chmodSync,
  closeSync,
  copyFileSync,
  existsSync,
  mkdirSync,
  openSync,
  readFileSync,
  realpathSync,
  renameSync,
  rmSync,
  statfsSync,
  statSync,
  symlinkSync,
  unlinkSync,
  writeFileSync,
} from "node:fs";
import path from "node:path";
import { iosOwnershipScope } from "./ownership-scope.mjs";
import { spawn, spawnSync } from "./background-process.mjs";
import { RightKitRegistryError, SNAPSHOT_ENV, SNAPSHOT_FILE, assertManifestRegistryEvidence, loadAdmittedSnapshot, prepareRightKitResolution, registryManifestFields, rightKitLockInventory, writeAdmissionRecord } from "./rightkit-registry-resolver.mjs";
import { fileURLToPath, pathToFileURL } from "node:url";
import { assertPrimaryReleaseCheckout, dirtyBuildInputs, resolveReleaseBuildInputs } from "./release-invocation.mjs";
import { resolveNativeCargoLayout } from "./native-cargo-layout.mjs";
import { admitSourceQa } from "./release-qa-audit.mjs";
import { devtoolsEvidence, resolveDevtoolsAllowance } from "./devtools-policy.mjs";
import { commandOutputPortable, releaseEnvironment, resolveReleaseLayout, runBuildStateMachine, verifySealedRelease, watchProgress } from "./release-state.mjs";
import { acquireCacheLease, acquireSuiteBuildSlot, applyCachePrune, assertWriteVolumeFloors, inspectCache, inspectWriteVolumes, markCacheEntrySuccessful, planCachePrune, readCachePolicy, resolveCacheLayout, resolveSharedCacheIdentity, resolveSharedCacheRoot } from "./cache-policy.mjs";
import { createTargetBridge } from "./target-bridge.mjs";
import { signingReceiptPath } from "./signing-receipts.mjs";
import { MEDIA_SIDECAR_RECEIPT_NAME, validateMediaSidecarConfig, validateMediaSidecarReceipt, windowsSidecarSigningProblems } from "./media-sidecar.mjs";
import { validateNotarizationReceipt } from "./macos-notarization.mjs";
import { brokerManagedHost } from "./cargo-contract.mjs";
import { ensureLegacyTargetLink, resolveReleaseCacheMode } from "./release-cache-mode.mjs";
import { resolveTargetRoot } from "./cargo-target.mjs";
import { assertNsisInPlaceUpgradeContract } from "./nsis-upgrade-contract.mjs";
import { collectSwiftToolVersions, planSwiftBuild } from "./swift-build-context.mjs";
import { planIosNativeBuild, resolveIosBuildContext } from "./ios-build-context.mjs";
import { packageCommandOptional, requiredBuildExecutables, resolveProjectType, targetUsesMsix } from "./project-type.mjs";
import { buildPathPrefix, collectPreflight, formatPreflight, preflightFailures } from "./preflight.mjs";
import { assertCleanSource } from "./source-gate.mjs";
import { acquireHeavyWorkSlot, heavyCommandEnvironment, summarizePreBrokerWaits, terminateProcessTree } from "./heavy-command.mjs";
import { processGroupCpuSeconds } from "./process-liveness.mjs";
import { cancelAndReapManagedRequest, firstManagedRequestId, releaseProgressLimits } from "./progress-control.mjs";
import { beginArtifactLease, completeArtifactLease, materializeArtifacts, retainArtifacts, resolveArtifactDir, resolveManagedRoot } from "./artifact-storage.mjs";
import { macosCodesignEnvironment } from "./codesign-environment.mjs";
import { ANDROID_RECEIPT_NAME, androidSealEntry, validateAndroidTarget, verifyAndroidReceipt } from "./android-release.mjs";
import { IOS_RECEIPT_NAME, iosSealEntry, verifyIosReceipt } from "./ios-package-recipe.mjs";
import { expandIosPackageTarget } from "./package-recipe-targets.mjs";
import { assertUpdaterSignature } from "./github-updater-release.mjs";

// The iOS lane's own packaging code, kept out of the win/mac fingerprint for the same reason as the Android one.
const IOS_PIPELINE_FINGERPRINT = createHash("sha256")
  .update(["ios-package-recipe.mjs", "ios-host.mjs"].map((file) => `${file}:${createHash("sha256").update(readFileSync(path.join(path.dirname(fileURLToPath(import.meta.url)), file))).digest("hex")}`).join("\n"))
  .digest("hex")
  .slice(0, 16);

// Fingerprint of the pipeline code that can change the bytes we ship or the way
// they are signed — deliberately NOT the package version, which moves for docs
// and test-only edits and would force a cold Rust rebuild of every app each time.
// Cached target directories must not outlive changes to these signing helpers.

const PIPELINE_FINGERPRINT_SOURCES = ["release.mjs", "sign-windows.mjs", "sign-macos.mjs", "macos-signing-session.mjs", "codesign-environment.mjs", "codesign-shim.mjs", "codesign-argv.mjs", "osascript-guard.mjs", "dmg-build.mjs", "dmg-layout.mjs", "dmg-spec.mjs", "swift-mac-dmg.mjs", "mac-bundle-signing.mjs", "media-sidecar.mjs", "media-sidecar-catalog.mjs", "mac-signing-matrix.mjs", "mac-sign-core.mjs", "macos-file-evidence.mjs", "swift-mac-app.mjs", "rightkit-signing-agent.mjs", "notarize-macos.mjs", "macos-notarization.mjs", "signing-receipts.mjs", "tauri-bundle-marker.mjs", "nsis-payload.mjs", "msix-build.mjs", "msix-plan.mjs"];
const PIPELINE_FINGERPRINT = createHash("sha256")
  .update(PIPELINE_FINGERPRINT_SOURCES.map((file) => `${file}:${createHash("sha256").update(readFileSync(path.join(path.dirname(fileURLToPath(import.meta.url)), file))).digest("hex")}`).join("\n"))
  .digest("hex")
  .slice(0, 16);
// The Android lane's own signing/scan code. Kept out of PIPELINE_FINGERPRINT_SOURCES so a change here never cold-rebuilds win/mac,
// but still part of the android signing identity so a changed scanner or receipt shape retires cached android output.
const ANDROID_PIPELINE_FINGERPRINT = createHash("sha256")
  .update(["android-release.mjs", "android-aab-scan.mjs", "android-ndk.mjs"].map((file) => `${file}:${createHash("sha256").update(readFileSync(path.join(path.dirname(fileURLToPath(import.meta.url)), file))).digest("hex")}`).join("\n"))
  .update(PIPELINE_FINGERPRINT)
  .digest("hex")
  .slice(0, 16);

/** Assemble preflight inputs from the app's own files (mirrors release.mjs). */
function buildPreflight({ config, configPath, appRoot, repoRoot, platform, nativeLayout, target, pinnedCargoLocks = [] }) {
  let version;
  try {
    version = JSON.parse(readFileSync(path.join(appRoot, "package.json"), "utf8")).version;
  } catch {
    version = undefined;
  }
  // An iOS target is built from its Xcode project and local packages only; the ownership scan covers just those roots.
  const ownershipScope = platform === "ios" ? iosOwnershipScope({ repoRoot, appRoot, target, nativeAssembly: config?.nativeAssembly }) : null;
  return collectPreflight({
    platform,
    ownershipScope,
    appRoot,
    repoRoot,
    app: config.app,
    configPath,
    // The version check is the doctor's job: by the time `build` runs, seal
    // collision is already enforced downstream, and duplicating it here would
    // reject a legitimate resume of an in-flight build.
    version: undefined,
    cargoLockPaths: [...(nativeLayout?.lockCandidates ?? []), ...pinnedCargoLocks],
    projectType: resolveProjectType({ appRoot, config, target }),
    // A native iOS app has no broker target or bridge; it still gets the Rust budget and lock checks when it pins a Cargo.lock.
    rust: Boolean(nativeLayout) || pinnedCargoLocks.length > 0,
    bridge: Boolean(nativeLayout),
    msix: targetUsesMsix(target),
  });
}

const TOOL_ROOT = path.dirname(fileURLToPath(import.meta.url));
const WORKER = path.join(TOOL_ROOT, "release.mjs");
const RELEASE_TOOL_VERSION = JSON.parse(readFileSync(path.join(TOOL_ROOT, "package.json"), "utf8")).version;
const args = process.argv.slice(2);
let configName = "right-release.config.mjs";
let platform = process.platform === "win32" ? "win" : process.platform === "darwin" ? "mac" : process.platform;
let dryRun = false;
let skipChecks = false;
let resumeFrom = null;

for (let i = 0; i < args.length; i += 1) {
  const arg = args[i];
  if (arg === "--config") configName = args[++i];
  else if (arg === "--platform") platform = args[++i];
  else if (arg === "--dry-run") dryRun = true;
  else if (arg === "--skip-checks") skipChecks = true;
  else if (arg === "--resume-from") resumeFrom = args[++i];
  else if (arg === "--tier" || arg.startsWith("--tier=")) fail("build is tier-neutral; select patch or update only during upload");
  else if (arg === "-h" || arg === "--help") usage(0);
  else fail(`unknown argument: ${arg}`);
}
if (platform !== "win" && platform !== "mac" && platform !== "android" && platform !== "ios") fail("--platform must be win, mac, android or ios");
if (resumeFrom && !["prepackage", "sign", "package", "post-sign", "harden"].includes(resumeFrom)) fail("--resume-from must be prepackage, sign, package, post-sign, or harden");

const configPath = path.resolve(configName);
const invocationRoot = path.dirname(configPath);
const repoRoot = git(invocationRoot, ["rev-parse", "--show-toplevel"]);
assertPrimaryReleaseCheckout(repoRoot);
// Native real paths: Git reports the long form of a Windows short name (RUNNER~1).
const nativeReal = (file) => { try { return realpathSync.native(file); } catch { return file; } };
const relativeConfig = path.relative(nativeReal(repoRoot), nativeReal(configPath)).replaceAll("\\", "/");
if (relativeConfig.startsWith("../")) fail("release config must live inside the Git repository");
const appRoot = path.dirname(configPath);
const layout = resolveReleaseLayout({ repoRoot, configPath });
const vaultRoot = layout.vaultRoot;
let lock;
let child = null;
let suiteSlot;
let cacheLease;
let heavySlot;
let interrupted;
let requestActiveRunStop = null;

// A dry-run is an inspection contract, not a partial build. It must finish
// before locks, cache directories, receipts, dependency installation, or any
// child build/sign process can exist.
if (dryRun) {
  const commit = git(invocationRoot, ["rev-parse", "HEAD"]);
  const dirtyConfig = dirtyBuildInputs(repoRoot, { include: [relativeConfig], required: [relativeConfig] }, commit);
  if (dirtyConfig.length > 0) fail(`dirty release config cannot be executed:\n${dirtyConfig.map((file) => `- ${file}`).join("\n")}`);
  const config = (await import(`${pathToFileURL(configPath).href}?dry-run=${commit}`)).default;
  if (!config?.app || !config?.version) fail("release config must expose app and version");
  const target = config.targets?.[platform];
  // Android builds with target.gradle (validated by release.mjs), not a package command; iOS with targets.ios.iosPackage.
  if (platform !== "android" && platform !== "ios" && !target?.package && !packageCommandOptional(target, resolveProjectType({ appRoot, config, target }))) fail(`${config.app} has no ${platform} package command`);
  console.log(`right-release build dry-run: ${config.app} ${config.version} ${platform}`);
  console.log(`config: ${configPath}`);
  console.log(`commit: ${commit}`);
  process.exit(0);
}

for (const signal of ["SIGINT", "SIGTERM", "SIGHUP"]) {
  process.once(signal, () => {
    interrupted = new Error(`release interrupted by ${signal}`);
    process.exitCode = signal === "SIGINT" ? 130 : 143;
    if (requestActiveRunStop) void requestActiveRunStop(interrupted.message);
    else if (child?.pid) killTree(child.pid);
  });
}

try {
  throwIfInterrupted();
  const commit = git(repoRoot, ["rev-parse", "HEAD"]);
  // A phase-resume never reads unrelated source. Its later build-input gate
  // remains strict, but requiring a globally clean mono-workspace here would
  // turn an already-built, signed installer into a needless full rebuild.
  if (!resumeFrom) {
    assertCleanSource({
      status: git(repoRoot, ["status", "--porcelain=v1", "-z", "--untracked-files=all"], { trim: false }), // untrimmed: a leading space is the first record's X column (#782)
      commandId: "right-release build",
    });
  }
  mkdirSync(path.join(vaultRoot, "locks"), { recursive: true });
  lock = acquireLock(path.join(vaultRoot, "locks", `${platform}.lock.json`));
  const shortCommit = commit.slice(0, 8);
  const dirtyConfig = dirtyBuildInputs(repoRoot, { include: [relativeConfig], required: [relativeConfig] }, commit);
  if (dirtyConfig.length > 0) fail(`dirty release config cannot be executed:\n${dirtyConfig.map((file) => `- ${file}`).join("\n")}`);
  const config = (await import(`${pathToFileURL(configPath).href}?commit=${commit}`)).default;
  if (!config?.app || !config?.version) fail("release config must expose app and version");
  if (!/^[A-Za-z0-9][A-Za-z0-9._-]*$/.test(config.app) || !/^[A-Za-z0-9][A-Za-z0-9._+-]*$/.test(config.version)) {
    fail("app and version must be safe release-id components");
  }
  const target = config.targets?.[platform];
  if (platform !== "android" && platform !== "ios" && !target?.package && !packageCommandOptional(target, resolveProjectType({ appRoot, config, target }))) fail(`${config.app} has no ${platform} package command`);
  // iOS: the same strict validation release.mjs applies; yields the IPA path and the team and bundle id the seal pins.
  let iosSpec = null;
  if (platform === "ios") {
    try { iosSpec = expandIosPackageTarget(target, { appRoot }).recipe?.spec ?? null; } catch (error) { fail(`${config.app} ios ${error.message}`); }
    if (!iosSpec) fail(`${config.app} ios must declare targets.ios.iosPackage`);
  }
  // Same validation release.mjs applies; yields the normalised upload-certificate pin and the AAB path.
  let androidTarget = null;
  if (platform === "android") {
    try { androidTarget = validateAndroidTarget(target, { root: appRoot }); } catch (error) { fail(`${config.app} android ${error.message}`); }
  }
  const isSwiftBuild = resolveProjectType({ appRoot, config, target }) === "swiftpm";
  // A native iOS app (xcodegen project; no Rust library unless nativeAssembly.cargoManifest declares one) has no src-tauri and no
  // declared nativeAssembly manifest: it owns no Cargo layout (null), and every Cargo-derived value below is skipped for it.
  // The Xcode tools are asserted present, then probed, inside resolveIosBuildContext (a missing tool fails with the normal message).
  const iosContext = iosSpec !== null
    ? resolveIosBuildContext({ appRoot, config, target, iosSpec, exec: (cmd, runArgs) => commandOutputAt(cmd, runArgs, appRoot), assertExecutables })
    : null;
  const nativeIos = iosContext?.nativeIos ?? false;
  const nativeLayout = iosContext ? iosContext.nativeLayout : resolveNativeCargoLayout({ appRoot, config });
  if (platform === "win" && target.signingContract === "windows-raw-exe-authenticode-before-nsis-v1") {
    assertNsisInPlaceUpgradeContract(appRoot, target.nsisUpgradeContract);
  }
  const { requiredInputs, buildInputs, cargoLockPaths: pinnedCargoLocks } = resolveReleaseBuildInputs({ repoRoot, appRoot, configPath, config, target });
  const dirtyInputs = dirtyBuildInputs(repoRoot, buildInputs, commit);
  if (dirtyInputs.length > 0) fail(`dirty files can change the packaged app; resolve them before release:\n${dirtyInputs.map((file) => `- ${file}`).join("\n")}`);
  const hasPackageJson = existsSync(path.join(appRoot, "package.json"));
  // iOS records the Xcode toolchain (and xcodegen when a project.yml is named) beside the Node and Rust tools.
  const toolVersions = isSwiftBuild ? collectSwiftToolVersions({ packageManager: config.packageManager, exec: commandOutput })
    : iosContext ? iosContext.toolVersions
    : collectToolVersions(config.packageManager ?? "pnpm", appRoot);
  const inputHashes = hashInputs(requiredInputs);
  const releaseId = `${config.app}-${config.version}-${shortCommit}`;
  const platformDir = platform === "win" ? "windows" : platform === "android" ? "android" : platform === "ios" ? "ios" : "mac";
  const stateRoot = path.join(vaultRoot, "state", releaseId, platformDir);
  const signingIdentity = platform === "win" ? {
    contract: target.signingContract ?? "<missing>",
    // Pipeline code participates in cache identity. Without it, a fix to the
    // signing or packaging logic leaves the previous logic's target directory
    // eligible for reuse — which is how outputs from a known-bad pipeline
    // outlived the change that was supposed to retire them.
    pipeline: PIPELINE_FINGERPRINT,
    configSha256: hashFile(configPath),
    // Resolve each phase receipt the same way signing does: the config's
    // sign.receipt / sign.installerReceipt override wins, the default
    // windows-<phase>.json name is only the fallback. Hardcoding the default
    // here made the seal read a path the signer never wrote whenever a config
    // (e.g. an architecture-qualified receipt) overrode it.
    receiptInputs: ["raw-exe", "installer", "embedding"].map((phase) => signingReceiptPath({ appRoot, target, platform, phase, stateRoot })),
  } : platform === "mac" ? {
    contract: target.signingContract ?? "<missing>",
    pipeline: PIPELINE_FINGERPRINT,
    configSha256: hashFile(configPath),
    // Mac portable releases produce one Developer ID receipt before packaging
    // and one Accepted notarization receipt after. Both must be part of cache
    // identity and seal evidence, including configured receipt overrides.
    receiptInputs: ["signing", "notarization"].map((phase) => signingReceiptPath({ appRoot, target, platform, phase, stateRoot })),
  } : platform === "android" ? {
    contract: target.signingContract ?? "<missing>",
    pipeline: ANDROID_PIPELINE_FINGERPRINT,
    configSha256: hashFile(configPath),
    // The pinned upload-certificate SHA-256 (64 lowercase hex). The seal refuses a receipt whose signer differs from it.
    uploadCertSha256: androidTarget.signing.expectedCertSha256,
    // release.mjs writes the AAB receipt into RIGHT_RELEASE_STATE_ROOT (= stateRoot) as android-aab.json.
    receiptInputs: [path.resolve(stateRoot, ANDROID_RECEIPT_NAME)],
  } : platform === "ios" ? {
    contract: "ios-xcodebuild-archive-automatic-v1",
    pipeline: IOS_PIPELINE_FINGERPRINT,
    configSha256: hashFile(configPath),
    // The team and bundle id the IPA receipt must carry; the upload re-checks both from the sealed manifest.
    teamId: iosSpec.teamId,
    bundleId: iosSpec.bundleId,
    // release.mjs writes the IPA receipt into RIGHT_RELEASE_STATE_ROOT (= stateRoot) as ios-archive.json.
    receiptInputs: [path.resolve(stateRoot, IOS_RECEIPT_NAME)],
  } : null;
  if (signingIdentity) inputHashes[".right-release/signing-identity.json"] = hashFileText(JSON.stringify(signingIdentity));
  const cargoLock = nativeLayout?.lockPath;
  const cargoToml = nativeLayout?.manifestPath;
  // A broker-managed host is not identified by RIGHTKIT_BUILD_BROKER_SOCKET alone:
  // that variable is exported by login shells only, while the managed cargo shim is
  // on PATH for every shell. Detecting on the variable alone made agent (non-login)
  // shells take the Cache V2 branch, bridge resolved native target into release cache,
  // and then lose CARGO_TARGET_DIR to the broker's unconditional override — so the
  // app bundle landed in the broker workspace and packaging failed on an empty cache.
  // swiftPlan is the "no Rust host" plan for SwiftPM and for a Cargo-less native iOS app alike: same shape, same skips.
  const swiftPlan = isSwiftBuild ? planSwiftBuild({ appRoot, vaultRoot, platform, target, toolVersions }) : nativeIos ? planIosNativeBuild({ appRoot, vaultRoot, platform, toolVersions }) : null;
  // SwiftPM targets have no Cargo manifest: no broker cargo target, no Rust cache identity.
  const managedCargoTarget = swiftPlan ? swiftPlan.managedCargoTarget : brokerManagedHost(process.env) ? resolveTargetRoot(cargoToml) : null;
  const cacheIdentity = swiftPlan ? swiftPlan.identity : resolveSharedCacheIdentity({ cargoLockPath: cargoLock, cargoTomlPath: cargoToml, rustcVerbose: toolVersions.rustc, targetTriple: target.cargoTarget ?? target.targetTriple ?? target.rustTarget ?? toolVersions.rustHost, profile: target.profile ?? "release" });
  const cacheKey = hashFileText(JSON.stringify({ cache: cacheIdentity.fingerprint, signingIdentity })).slice(0, 16);
  const cacheModeRequested = swiftPlan ? swiftPlan.cacheMode : resolveReleaseCacheMode({ requested: process.env.RIGHT_RELEASE_CACHE_MODE, platform, env: process.env });
  // On a broker-managed host, Cache V2's lease/slot/prune/floor/mark chain is not
  // running against the directory that will actually hold build output — the
  // broker owns CARGO_TARGET_DIR unconditionally (see managedCargoTarget above).
  // Leaving cacheMode at "shared" there ran that whole chain against an
  // essentially unrelated cache root: acquireSuiteBuildSlot/acquireCacheLease
  // guarded a lock nothing else respects, and assertWriteVolumeFloors could
  // fail() the build outright on a volume that isn't where the real ~18 GB of
  // output lives. Force broker so none of that machinery runs.
  const cacheMode = managedCargoTarget ? "broker" : cacheModeRequested;
  if (managedCargoTarget && cacheModeRequested === "shared") {
    console.error("[right-release] broker-managed host: the build broker owns CARGO_TARGET_DIR; Cache V2 shared-cache lease/prune/floor checks skipped");
  }
  const sharedCacheRoot = cacheMode === "shared" ? resolveSharedCacheRoot({ platform, env: process.env }) : undefined;
  const sharedLayout = cacheMode === "shared"
    ? resolveCacheLayout({ cacheRoot: sharedCacheRoot, platform, architecture: cacheIdentity.architecture, app: config.app, fingerprint: cacheKey, kind: "release" })
    : undefined;
  if (cacheMode === "shared") assertSccacheVersion(toolVersions.sccache);

  // Preflight BEFORE any compile. These preconditions used to be discovered one
  // per build cycle, minutes deep, with errors that named a symptom instead of a
  // cause. Blocking problems stop here, in seconds, naming the fix.
  const preflight = buildPreflight({ config, configPath, appRoot, repoRoot: layout.repoRoot, platform, nativeLayout, target, pinnedCargoLocks });
  const preflightBlockers = preflightFailures(preflight);
  if (preflightBlockers.length > 0) {
    fail(`preflight found ${preflightBlockers.length} blocking problem(s):\n${formatPreflight(preflightBlockers)}`);
  }
  const ownershipEvidence = preflight.find((check) => check.name === "ownership")?.evidence;
  if (!ownershipEvidence) fail("ownership evidence was not produced by release preflight");
  if (ownershipEvidence.commit !== commit) {
    fail(`ownership scan commit ${ownershipEvidence.commit ?? "(missing)"} differs from release commit ${commit}; regenerate with right-release ownership at release commit ${commit}`);
  }
  for (const check of preflight) {
    if (check.status !== "ok") console.error(`[right-release] preflight ${check.name}: ${check.detail}`);
  }
  // Hardening step 4: source QA is recorded, not enforced. A missing or rejected receipt warns loudly and the outcome
  // is sealed into the manifest (`qa.status`) for `right-release qa audit`. Dev lanes (install-app --dev) never reach this file.
  const qaAdmission = admitSourceQa({ repoRoot: layout.repoRoot, releaseId, platform, configName: path.relative(process.cwd(), configPath) || path.basename(configPath), releaseConfigSha256: hashFile(configPath) });

  // A tool being INSTALLED is not the same as it being the one PATH picks. The
  // vendored-OpenSSL build shells out to `perl`, and under Git Bash/MSYS that
  // resolves to MSYS Perl, which cannot configure OpenSSL. Put the real Windows
  // Perl first so the build behaves the same from any shell.
  const pathPrefix = buildPathPrefix({ platform });
  const baseEnv = macosCodesignEnvironment({
    ...heavyCommandEnvironment(process.env),
    ...releaseEnvironment({ root: vaultRoot, cacheRoot: sharedCacheRoot, platform, architecture: cacheIdentity.architecture, app: config.app, cacheKey, kind: "release", appRoot, mode: cacheMode }),
    ...(pathPrefix ? { PATH: `${pathPrefix}${path.delimiter}${process.env.PATH ?? ""}` } : {}),
    RIGHT_RELEASE_REPO_ROOT: layout.repoRoot,
    RIGHT_RELEASE_APP_ROOT: layout.appRoot,
    RIGHTKIT_MANAGED_ROOT: resolveManagedRoot({ env: process.env }),
    RIGHT_RELEASE_ARTIFACT_DIR: resolveArtifactDir({ app: config.app, version: config.version, managedRoot: resolveManagedRoot({ env: process.env }) }),
  });
  const env = swiftPlan ? swiftPlan.sanitizeEnv(baseEnv) : baseEnv;
  if (managedCargoTarget) {
    for (const name of ["CARGO_HOME", "CARGO_TARGET_DIR", "RUSTC_WRAPPER", "SCCACHE_DIR", "SCCACHE_BASEDIRS", "SCCACHE_CACHE_SIZE", "RUSTFLAGS"]) {
      delete env[name];
    }
  }
  if (cacheMode !== "shared") delete env.RIGHT_RELEASE_CACHE_OWNER;
  if (cacheMode === "shared" && env.RIGHT_RELEASE_CACHE_OWNER !== "rightkit-v2") fail("shared cache ownership token was not configured");
  if (cacheMode !== "shared" && !commandExists("sccache")) delete env.RUSTC_WRAPPER;
  if (process.env.RIGHTSUITE_HEAVY_WORK_OWNER !== "1") heavySlot = acquireHeavyWorkSlot();
  if (cacheMode === "shared") {
    suiteSlot = acquireSuiteBuildSlot({ layout: sharedLayout, pid: process.pid, argv: process.argv, waitMs: Number(process.env.BUILD_GUARD_WAIT_SECS ?? 900) * 1000 });
    cacheLease = acquireCacheLease({ layout: sharedLayout, pid: process.pid, argv: process.argv });
  }
  // Pre-broker wait attribution: every layer, ms, and blocker identity (Step 1 instrumentation).
  const preBrokerWaits = summarizePreBrokerWaits({ heavySlot, suiteSlot, heavyOwnerInherited: process.env.RIGHTSUITE_HEAVY_WORK_OWNER === "1" });
  console.error(`[right-release] pre-broker waits: ${preBrokerWaits.summary}`);
  throwIfInterrupted();
  const managedRoot = resolveManagedRoot({ env: process.env });
  const managedArtifactDir = resolveArtifactDir({ app: config.app, version: config.version, managedRoot });
  beginArtifactLease({ app: config.app, version: config.version, managedRoot });
  env.RIGHTKIT_MANAGED_ROOT = managedRoot;
  env.RIGHT_RELEASE_ARTIFACT_DIR = managedArtifactDir;
  const buildRoot = managedCargoTarget ? null : path.join(vaultRoot, "build", config.app, config.version, shortCommit, platformDir);
  env.RIGHT_RELEASE_STATE_ROOT = stateRoot;
  // Resolve the exact locked RightKit pins against the canonical registries ONCE, here (build admission), and keep
  // the snapshot in this release's state root with an admission record naming the release (id, commit, platform) and
  // the snapshot digest. The release worker, a --resume-from phase resume and the sealed manifest consume only that
  // recorded snapshot; a missing one is refused, never re-resolved. These are trusted-local files: a resume proves the
  // recorded admission is consistent with this release and the current locks, not that the registry still agrees nor
  // that the controller wrote it (see rightkit-registry-resolver.mjs, TRUST SCOPE). Lock inputs come from the canonical build-input
  // inventory (requiredInputs, pinnedCargoLocks), not a reconstructed list.
  mkdirSync(stateRoot, { recursive: true });
  const rightKitSnapshotFile = path.join(stateRoot, SNAPSHOT_FILE);
  const admissionIdentity = { releaseId, commit, platform };
  const sealedManifestPath = path.join(repoRoot, ".right-release", "sealed", releaseId, platformDir, "release-manifest.json");
  const sealedExists = existsSync(sealedManifestPath);
  const rightKitReuse = Boolean(resumeFrom) || sealedExists;
  let boundSnapshot = null;
  let legacySealed = false;
  try {
    if (sealedExists) {
      const sealedManifest = JSON.parse(readFileSync(sealedManifestPath, "utf8"));
      legacySealed = assertManifestRegistryEvidence(sealedManifest).legacy;
      boundSnapshot = legacySealed ? null : sealedManifest.rightkitRegistry.snapshot;
    } else if (resumeFrom) {
      boundSnapshot = loadAdmittedSnapshot(stateRoot, { ...admissionIdentity, inPreflight: true });
    }
  } catch (error) {
    fail(`${error instanceof RightKitRegistryError && error.recoverable ? "RECOVERABLE " : ""}${error.message}`);
  }
  let rightKitResolution = null;
  if (!legacySealed) {
    try {
      rightKitResolution = await prepareRightKitResolution({
        root: appRoot,
        extraLocks: rightKitLockInventory({ requiredInputs, cargoLockPaths: pinnedCargoLocks, nativeLayout, config, root: appRoot }),
        versions: JSON.parse(readFileSync(path.join(TOOL_ROOT, "rightkit-versions.json"), "utf8")),
        ownership: JSON.parse(readFileSync(path.join(TOOL_ROOT, "rightkit-ownership.json"), "utf8")),
        releaseVersion: RELEASE_TOOL_VERSION,
        snapshotFile: rightKitSnapshotFile,
        boundSnapshot,
        reuse: rightKitReuse,
        // Build admission: a fresh build samples registry latest live and refuses EVERY pin behind it (a hold is a lane
        // task, not an exception). The snapshot is then frozen through build, seal, resume and upload.
        admission: !rightKitReuse,
        write: true,
      });
    } catch (error) {
      if (!(error instanceof RightKitRegistryError)) throw error;
      fail(`${error.recoverable ? "RECOVERABLE " : ""}${error.message}`);
    }
    for (const line of rightKitResolution.freshness.tasks) console.error(`[right-release] LANE TASK ${line}`);
    for (const line of rightKitResolution.freshness.warnings) console.error(`[right-release] WARNING ${line}`);
    console.error(`[right-release] rightkit-registry: ${rightKitResolution.summary}`);
    if (rightKitReuse) console.error("[right-release] resume reuses the recorded admission snapshot: it is checked for consistency with this release and the current locks only; the registry is not asked again and the record is not authenticated");
    env[SNAPSHOT_ENV] = rightKitSnapshotFile;
  }
  env.RIGHT_RELEASE_VAULT_STATE_ROOT = path.join(vaultRoot, "state");
  const lockPayload = {
    schema: 1,
    releaseId,
    app: config.app,
    version: config.version,
    commit,
    platform,
    cacheKey,
    inputs: inputHashes,
    signingIdentity,
    tools: toolVersions,
    createdAt: new Date().toISOString(),
  };
  const targetLink = nativeLayout?.targetLink;
  // ownedRoot is the per-app cache parent holding every fingerprint dir, so a
  // link left by an earlier fingerprint is recognised as ours and repointed
  // instead of hard-stopping the build.
  const targetBridge = swiftPlan ? swiftPlan.targetBridge : createTargetBridge({
    link: targetLink,
    target: managedCargoTarget ?? env.CARGO_TARGET_DIR,
    ownedRoot: path.dirname(managedCargoTarget ?? env.CARGO_TARGET_DIR),
    brokerManaged: Boolean(managedCargoTarget),
  });

  const result = await targetBridge.run(async () => runBuildStateMachine({
    root: repoRoot,
    app: config.app,
    version: config.version,
    commit,
    platform,
    requiredInputs,
    inputHashes,
    cacheKey,
    ops: {
      preflight: async () => {
        throwIfInterrupted();
        if (cacheMode === "shared") {
          const policy = readCachePolicy(process.env);
          const snapshot = inspectCache({ cacheRoot: sharedCacheRoot });
          const plan = planCachePrune({ snapshot, policy, protectedEntryIds: new Set([sharedLayout.id]) });
          applyCachePrune({ plan, cacheRoot: sharedCacheRoot, dryRun: false });
          const volumes = inspectWriteVolumes({ repoRoot, vaultRoot, cacheRoot: sharedCacheRoot });
          assertWriteVolumeFloors({ volumes, policy, configuredRepoMinimumBytes: Math.max(Number(target.preflight?.minFreeGb ?? 25), 25) * 1024 ** 3 });
        } else assertDiskSpace(repoRoot, target.preflight?.minFreeGb ?? 20);
        assertExecutables([...requiredBuildExecutables({ projectType: resolveProjectType({ appRoot, config, target }), packageManager: config.packageManager ?? "pnpm", usesPackageManager: nativeIos ? hasPackageJson : Boolean(config.packageManager) || hasPackageJson, usesCargo: !nativeIos }), ...(iosContext?.prepareExecutables ?? []), ...(target.preflight?.executables ?? [])]);
        for (const name of target.preflight?.env ?? []) if (!process.env[name]) fail(`missing required environment variable: ${name}`);
        for (const command of target.preflight?.commands ?? []) runChecked(command.cmd, command.args ?? [], path.resolve(appRoot, command.cwd ?? "."), env);
        if (buildRoot) mkdirSync(buildRoot, { recursive: true });
        mkdirSync(stateRoot, { recursive: true });
        writeJson(path.join(stateRoot, "release-waits.json"), { schema: 1, recordedAt: new Date().toISOString(), ...preBrokerWaits });
        if (buildRoot) writeJson(path.join(buildRoot, "release-inputs.lock.json"), lockPayload);
        writeJson(path.join(stateRoot, "release-inputs.lock.json"), lockPayload);
        // The controller's admission record: which snapshot was admitted for exactly this release and input identity.
        if (rightKitResolution) writeAdmissionRecord(stateRoot, { ...admissionIdentity, snapshot: rightKitResolution.snapshot });
        checkpoint(stateRoot, "preflight_complete");
      },
      prepare: async () => {
        throwIfInterrupted();
        const cacheTarget = swiftPlan ? swiftPlan.buildDir : managedCargoTarget ?? env.CARGO_TARGET_DIR;
        mkdirSync(cacheTarget, { recursive: true });
        // Broker-managed hosts route through the same target bridge as shared
        // mode even though cacheMode is forced to "broker" above: the naive
        // else-branch below fail()s outright when targetLink is a real
        // directory, which is exactly the "corruption" misreading that nearly
        // cost ~5.9 GB of warm broker intermediates. The bridge's brokerManaged
        // flag already knows to leave a real directory untouched instead.
        if (swiftPlan) { /* SwiftPM: no Rust target link */ }
        else if (cacheMode === "shared" || managedCargoTarget) targetBridge.ensure();
        else {
          const linked = ensureLegacyTargetLink({ targetLink, cacheTarget, fs: { existsSync, realpathSync, renameSync, symlinkSync } });
          if (linked.action === "refused") fail(linked.reason);
          if (linked.action === "moved-aside") console.error(`[right-release] hosted CI: moved existing ${targetLink} aside to ${linked.aside}`);
        }
        // A Cargo-less native iOS app installs Node dependencies only when it has a package.json.
        if (nativeIos ? hasPackageJson : resolveProjectType({ appRoot, config, target }) !== "swiftpm" || config.packageManager) await runProgress(config.packageManager ?? "pnpm", ["install", "--frozen-lockfile"], appRoot, env, cacheTarget, path.join(stateRoot, "stall-install.log"));
      },
      build: async () => {
        throwIfInterrupted();
        await runProgress(
          process.execPath,
          [WORKER, "--config", configPath, "--platform", platform, "--no-upload", "--resume", ...(resumeFrom ? ["--resume-from", resumeFrom] : []), ...(skipChecks ? ["--skip-checks"] : [])],
          appRoot,
          env,
          swiftPlan ? swiftPlan.watchDirs : [managedCargoTarget ?? env.CARGO_TARGET_DIR, path.join(appRoot, ".cache")],
          path.join(stateRoot, "stall-build.log"),
          { livenessOwnerTimeoutMs: target.package?.livenessOwnerTimeoutMs ?? null },
        );
        checkpoint(stateRoot, "build_complete");
        checkpoint(stateRoot, "signed");
        // Truthful checkpoint: the worker (release.mjs) runs the hardening scan only when it has scan
        // targets and fails the whole build when the scan fails, so reaching here with targets means
        // the scan passed. With no targets the worker ran no scan — never claim "hardened".
        if (hardeningScanTargets(target).length > 0) checkpoint(stateRoot, "hardened");
      },
      // Product-specific checks that must run after the signed/hardened bytes
      // exist but before they become immutable. This is intentionally a small
      // hook: Right Release still owns signing, hardening, and sealing; apps
      // may only inspect their candidate and emit a pass/fail result.
      preSeal: async ({ sealedDir }) => {
        if (!target.preSeal) return;
        await runConfiguredReleaseCheck(target.preSeal, {
          root: appRoot,
          env: {
            RIGHT_RELEASE_SEALED_DIR: sealedDir,
            RIGHT_RELEASE_RELEASE_ID: releaseId,
            RIGHT_RELEASE_PLATFORM: platform,
          },
        });
      },
      seal: async ({ sealedDir }) => {
        sealRelease({ configRoot: appRoot, managedCargoTarget, androidTarget, iosSpec, nativeLayout, sealedDir, releaseId, config, target, platform, commit, inputHashes, toolVersions, cacheKey, signingIdentity, ownershipEvidence, qaAdmission, managedRoot, stateRoot, rightkitRegistryFields: rightKitResolution ? registryManifestFields(rightKitResolution.snapshot, RELEASE_TOOL_VERSION) : undefined });
        verifySealedRelease(sealedDir);
        if (cacheMode === "shared") markCacheEntrySuccessful({ layout: sharedLayout });
        checkpoint(stateRoot, "sealed");
      },
    },
  }));
  if (cacheMode === "shared") {
    const snapshot = inspectCache({ cacheRoot: sharedCacheRoot });
    const plan = planCachePrune({ snapshot, policy: readCachePolicy(process.env), protectedEntryIds: new Set([sharedLayout.id]) });
    applyCachePrune({ plan, cacheRoot: sharedCacheRoot, dryRun: false });
  }
  console.log(`right-release build: ${result.resumed ? "resumed" : "sealed"} ${result.releaseId}`);
  console.log(`sealed: ${result.sealedDir}`);
} finally {
  cacheLease?.release();
  suiteSlot?.release();
  heavySlot?.release();
  lock?.release();
}

function hashInputs(files) {
  const hashes = {};
  for (const file of files) {
    const key = path.relative(path.dirname(path.dirname(file)), file);
    // Two inputs (an app file and a shared crate's file, say) must never share a key: one would silently replace the other.
    if (Object.hasOwn(hashes, key)) fail(`release inputs share the hash key ${key}; rename one so every input is sealed`);
    hashes[key] = hashFile(file);
  }
  return hashes;
}

function hashFile(file) {
  return createHash("sha256").update(readFileSync(file)).digest("hex");
}

function hashFileText(text) {
  return createHash("sha256").update(text).digest("hex");
}

function sqlCipherFeatures(text) {
  return [...text.matchAll(/features\s*=\s*\[([^\]]+)\]/g)]
    .flatMap((match) => match[1].match(/"([^"]+)"/g) ?? [])
    .map((value) => value.slice(1, -1))
    .filter((value) => /sqlcipher|openssl/i.test(value));
}

function sealRelease({ configRoot, managedCargoTarget, androidTarget, iosSpec, nativeLayout, sealedDir, releaseId, config, target, platform, commit, inputHashes, toolVersions, cacheKey, signingIdentity, ownershipEvidence, qaAdmission, managedRoot, stateRoot, rightkitRegistryFields }) {
  // Every app's right-release.config.mjs names installer/updater artifacts
  // relative to its resolved native target. On a broker-managed host that
  // directory is never populated (the broker owns CARGO_TARGET_DIR and
  // targetBridge.ensure() deliberately leaves a real local directory alone,
  // see target-bridge.mjs) so those paths must resolve against the broker's
  // own target root instead, or every managed build fails at seal after a
  // full compile, sign, and notarize.
  const targetPrefix = "src-tauri/target/";
  // A Cargo-less iOS app has no native layout and no managed cargo target: only the default prefix is ever compared.
  const effectiveTargetPrefix = nativeLayout?.targetPrefix ?? targetPrefix;
  const resolveArtifactPath = (file) => {
    const normalized = file.replaceAll(path.win32.sep, "/");
    if (managedCargoTarget && normalized.startsWith(targetPrefix)) {
      return path.join(managedCargoTarget, normalized.slice(targetPrefix.length));
    }
    if (managedCargoTarget && effectiveTargetPrefix !== targetPrefix && normalized.startsWith(effectiveTargetPrefix)) {
      return path.join(managedCargoTarget, normalized.slice(effectiveTargetPrefix.length));
    }
    const local = path.resolve(configRoot, file);
    if (existsSync(local)) return local;
    const managed = resolveArtifactDir({ app: config.app, version: config.version, managedRoot });
    const managedCandidate = path.join(managed, path.basename(file));
    if (existsSync(managedCandidate)) return managedCandidate;
    return local;
  };
  const sources = new Map();
  const addSource = (source, role) => {
    const current = sources.get(source) ?? new Set();
    current.add(role);
    sources.set(source, current);
  };
  for (const artifact of target.installer?.artifacts ?? []) addSource(resolveArtifactPath(artifact.file), "installer");
  for (const artifact of target.updater?.artifacts ?? []) {
    addSource(resolveArtifactPath(artifact.file), "updater");
    if (target.updater?.kind !== "appinstaller") addSource(resolveArtifactPath(artifact.signature), "updater-signature");
  }
  // Portable contracts ship bare archives (zip / tar.gz) with no installer or
  // updater block: `target.artifacts` entries (paths or { file }) seal with the
  // "portable" role. On android the declared AAB seals with the "aab" role instead.
  const aabSource = platform === "android" ? resolveArtifactPath(androidTarget.aab.file) : null;
  const ipaSource = platform === "ios" ? resolveArtifactPath(iosSpec.ipa) : null;
  for (const artifact of target.artifacts ?? []) {
    const file = typeof artifact === "string" ? artifact : artifact?.file;
    if (typeof file !== "string" || !file) fail("target.artifacts entries must be paths or { file }");
    const source = resolveArtifactPath(file);
    addSource(source, source === aabSource ? "aab" : source === ipaSource ? "ipa" : "portable");
  }
  if (sources.size === 0) fail(`nothing to seal: declare installer.artifacts, updater.artifacts or artifacts for the ${platform} target`);
  for (const file of sources.keys()) if (!existsSync(file)) fail(`cannot seal missing artifact: ${file}`);
  // Every minisign updater requires an explicit trust root & full-byte signature
  // verification before any seal directory or artifact copy is created.
  const updaterPublicKey = config.distribution?.updaterPublicKey ?? config.distribution?.updaterPubkey ?? target?.updater?.pubkey ?? target?.updater?.publicKey;
  if ((target.updater?.artifacts?.length ?? 0) > 0 && target.updater?.kind !== "appinstaller") {
    if (typeof updaterPublicKey !== "string" || !updaterPublicKey.trim()) fail("updater public key is required to verify updater artifacts before sealing");
    for (const artifact of target.updater?.artifacts ?? []) {
      const source = resolveArtifactPath(artifact.file);
      const signature = readFileSync(resolveArtifactPath(artifact.signature), "utf8").trim();
      if (!signature) fail(`updater signature is empty, refusing to seal: ${artifact.signature}`);
      try { assertUpdaterSignature(readFileSync(source), signature, updaterPublicKey, `signature of ${path.basename(artifact.file)}`); }
      catch (error) { fail(`updater signature rejected at seal: ${error.message}`); }
    }
  }
  // Android: the build receipt must describe exactly the AAB being sealed (bytes re-hashed) and its signer must be the pinned
  // upload certificate. Checked before anything is copied, so a mismatch leaves no partial seal.
  let androidReceipt = null;
  if (platform === "android") {
    if (!sources.has(aabSource)) fail("android seal requires targets.android.artifacts to list aab.file");
    const [receiptPath] = signingIdentity.receiptInputs;
    if (!existsSync(receiptPath)) fail(`no signing receipts found to seal; expected at least ${receiptPath}`);
    try { androidReceipt = verifyAndroidReceipt(receiptPath, aabSource); } catch (error) { fail(`android AAB receipt rejected: ${error.message}`); }
    if (androidReceipt.signer?.certSha256 !== signingIdentity.uploadCertSha256) fail("android AAB receipt signer certificate is not the pinned upload certificate");
  }
  // iOS: the receipt must describe exactly the IPA being sealed (bytes re-hashed), for the pinned team and bundle id.
  let iosReceipt = null;
  if (platform === "ios") {
    if (!sources.has(ipaSource)) fail("ios seal requires targets.ios.artifacts to list iosPackage.ipa");
    const [receiptPath] = signingIdentity.receiptInputs;
    if (!existsSync(receiptPath)) fail(`no signing receipts found to seal; expected at least ${receiptPath}`);
    try { iosReceipt = verifyIosReceipt(receiptPath, ipaSource); } catch (error) { fail(`ios IPA receipt rejected: ${error.message}`); }
    if (iosReceipt.teamId !== signingIdentity.teamId || iosReceipt.bundleId !== signingIdentity.bundleId) fail("ios IPA receipt team or bundle id is not the one pinned by the release config");
    if (iosReceipt.sourceCommit !== commit) fail(`ios IPA receipt was built from ${String(iosReceipt.sourceCommit)}, not the release commit ${commit}`);
  }
  // rightkit-ffmpeg decode sidecar: release.mjs signed the binaries with the app and wrote media-sidecar.json. The seal
  // refuses a release without it, re-checks its shape against the configuration, and (Windows) that every sidecar's
  // signed digest is one the sealed raw-EXE signing receipt recorded. The hashes then live in the sealed manifest.
  let mediaSidecars = null;
  if (target.mediaSidecar !== undefined) {
    const receiptPath = path.resolve(stateRoot, MEDIA_SIDECAR_RECEIPT_NAME);
    if (!existsSync(receiptPath)) fail(`mediaSidecar is configured but its receipt is missing: ${receiptPath}`);
    try {
      const sidecarConfig = validateMediaSidecarConfig(target.mediaSidecar, { platform });
      const receipt = validateMediaSidecarReceipt(JSON.parse(readFileSync(receiptPath, "utf8")), { os: sidecarConfig.os, variant: sidecarConfig.variant, tools: sidecarConfig.tools });
      if (receipt.arch !== sidecarConfig.arch) throw new Error(`receipt arch ${receipt.arch} differs from the configured ${sidecarConfig.arch}`);
      if (platform === "win") {
        const rawReceipt = JSON.parse(readFileSync(signingIdentity.receiptInputs[0], "utf8"));
        const problems = windowsSidecarSigningProblems(rawReceipt, receipt);
        if (problems.length) throw new Error(problems.join("; "));
      }
      mediaSidecars = { ...receipt, receiptSha256: hashFile(receiptPath) };
    } catch (error) { fail(`media sidecar receipt rejected: ${error.message}`); }
  }
  const temp = `${sealedDir}.tmp-${process.pid}`;
  if (existsSync(temp)) rmSync(temp, { recursive: true, force: true });
  mkdirSync(temp, { recursive: true });
  const files = [];
  const artifactInputs = [];
  const sourceNames = new Map();
  for (const [source, roles] of sources) {
    let name = path.basename(source);
    if ([...sourceNames.values()].includes(name) && sourceNames.get(source) !== name) name = `${[...roles].join("-")}-${name}`;
    sourceNames.set(source, name);
    copyFileSync(source, path.join(temp, name));
    files.push(androidReceipt && source === aabSource && name === androidReceipt.file.name
      ? androidSealEntry(androidReceipt)
      : iosReceipt && source === ipaSource && name === iosReceipt.file.name
      ? iosSealEntry(iosReceipt)
      : { role: [...roles].sort().join("+"), name, sha256: hashFile(source), sizeBytes: statSync(source).size });
    artifactInputs.push({ source, name });
  }
  const routes = { patch: [], update: [] };
  for (const artifact of target.installer?.artifacts ?? []) {
    const source = resolveArtifactPath(artifact.file);
    routes.patch.push({ role: "installer", name: sourceNames.get(source), bucket: "public", key: artifact.key });
  }
  for (const artifact of target.updater?.artifacts ?? []) {
    const source = resolveArtifactPath(artifact.file);
    const signatureSource = target.updater?.kind === "appinstaller" ? null : resolveArtifactPath(artifact.signature);
    // github-releases updater artifacts declare no R2 key: the asset name is the route key and nothing is replaced in place.
    const githubUpdater = config.distribution?.provider === "github-releases";
    const routeKey = artifact.key ?? (githubUpdater ? path.basename(artifact.file) : undefined);
    const patchKey = artifact.patchKey ?? routeKey.replace("/updates/", "/installers/");
    routes.patch.push({ role: "updater", name: sourceNames.get(source), signature: signatureSource ? sourceNames.get(signatureSource) : undefined, bucket: "public", key: patchKey, platform: artifact.platform });
    routes.update.push({ role: "updater", name: sourceNames.get(source), signature: signatureSource ? sourceNames.get(signatureSource) : undefined, bucket: "private", key: routeKey, platform: artifact.platform });
  }
  // Derived from the worker's actual scan, never asserted unconditionally: release.mjs scans exactly
  // `target.hardening ?? target.artifacts` and fails the build on scan failure, so a completed build with
  // targets passed; with no targets no scan ran and the status is "skipped", not passed.
  const hardeningTargets = hardeningScanTargets(target);
  const hardeningScanned = hardeningTargets.length > 0;
  writeJson(path.join(temp, "hardening.json"), { schema: 1, status: hardeningScanned ? "passed" : "skipped", passed: hardeningScanned, checkedAt: new Date().toISOString(), targets: hardeningTargets.length, files: hardeningScanned ? files.map((file) => file.sha256) : [] });
  const manifest = {
    schema: 1,
    releaseId,
    app: config.app,
    version: config.version,
    commit,
    ownershipEvidence,
    // The exact registry resolution (identities, digests, registries, policy revision, observation time, observed latest)
    // this release was built against. Upload and resume consume it; neither resolves again.
    ...rightkitRegistryFields,
    qa: qaAdmission,
    // DevTools allowance (owner's fixed design): { mode: "denied" } for every app; content-only only for an allow-listed app.
    devtools: devtoolsEvidence(resolveDevtoolsAllowance({ config, root: configRoot })),
    platform,
    ...(mediaSidecars ? { mediaSidecars } : {}),
    cacheKey,
    // Portable contracts sign only raw executables — installer/embedding
    // receipts legitimately never exist there. Seal every receipt that was
    // produced, but never zero: a signed platform with no receipt at all is
    // an unsound seal, not a portable release.
    signing: signingIdentity ? { ...signingIdentity, receipts: (() => {
      const present = signingIdentity.receiptInputs.filter((file) => existsSync(file));
      if (present.length === 0) throw new Error(`no signing receipts found to seal; expected at least ${signingIdentity.receiptInputs[0]}`);
      if (platform === "mac") {
        if (present.length !== signingIdentity.receiptInputs.length) throw new Error("missing current-build macOS signing or notarization receipt");
        validateNotarizationReceipt({ receiptPath: signingIdentity.receiptInputs[1], archive: resolveArtifactPath(target.notarize.file) });
      }
      return present.map((file) => ({ file, sha256: hashFile(file) }));
    })() } : null,
    files,
    routes,
    inputs: inputHashes,
    tools: toolVersions,
    checkpoints: ["preflight_complete", "build_complete", "signed", ...(hardeningScanned ? ["hardened"] : []), "sealed"],
    sealedAt: new Date().toISOString(),
  };
  writeJson(path.join(temp, "release-manifest.json"), manifest);
  if (existsSync(sealedDir)) fail(`sealed directory already exists: ${sealedDir}`);
  mkdirSync(path.dirname(sealedDir), { recursive: true });
  renameSync(temp, sealedDir);
  for (const item of files) {
    const file = path.join(sealedDir, item.name);
    try { chmodSync(file, 0o444); } catch { /* best effort; hashes are authoritative */ }
  }
  // Keep candidate installer/updater bytes outside app repositories. Sealed
  // copies remain immutable release authority; managed copies support local
  // retention and recovery without reintroducing repo-root artifacts.
  materializeArtifacts({ app: config.app, version: config.version, managedRoot, files: artifactInputs });
  completeArtifactLease({ app: config.app, version: config.version, managedRoot });
  const retention = retainArtifacts({ app: config.app, managedRoot });
  if (retention.state === "unknown") console.error(`[right-release] artifact retention skipped: ${retention.reason}`);
  else if (retention.removed.length) console.log(`[right-release] artifact retention removed ${retention.removed.join(", ")}`);
}

/** The exact scan-target expression the worker uses (release.mjs: `target.hardening ?? target.artifacts ?? []`).
 * Recipe expansion in the worker preserves both fields (`...rest`), so the raw config target gives the same list. */
function hardeningScanTargets(target) {
  return target?.hardening ?? target?.artifacts ?? [];
}

// Probes run with cwd = the app root (bogusyogi/rightkit#783): on an agent host `cargo` is the
// RustRight shim, which refuses any cwd outside a primary checkout, so a $TMPDIR probe dir failed
// with RK_NOT_PRIMARY_CHECKOUT before the first build step. The app root also makes rustc report
// the toolchain the app's rust-toolchain.toml selects, which is the one the build uses.
function collectToolVersions(packageManager, probeDir = process.cwd()) {
  const rustcVerbose = commandOutputAt("rustc", ["-vV"], probeDir);
  return {
    node: process.version,
    packageManager: `${packageManager} ${commandOutput(packageManager, ["--version"])}`,
    cargo: commandOutputAt("cargo", ["--version"], probeDir),
    rustc: rustcVerbose,
    rustHost: rustcVerbose.match(/^host:\s*(.+)$/m)?.[1] ?? "unknown",
    sccache: commandExists("sccache") ? commandOutput("sccache", ["--version"]) : null,
  };
}

async function runProgress(cmd, runArgs, cwd, env, watchDir, diagnosticPath = null, { livenessOwnerTimeoutMs = null } = {}) {
  const { inactivityMs, absoluteMs } = releaseProgressLimits(env, { livenessOwnerTimeoutMs });
  let lastProgress = Date.now();
  const watchDirs = Array.isArray(watchDir) ? watchDir : [watchDir];
  let lastMtime = Math.max(...watchDirs.map(newestMtime));
  const started = Date.now();
  await new Promise((resolve, reject) => {
    let settled = false;
    let stopping = false;
    let managedRequestId = null;
    const settleResolve = () => { if (!settled) { settled = true; resolve(); } };
    const settleReject = (error) => { if (!settled) { settled = true; reject(error); } };
    const useShell = process.platform === "win32" && !/\.(?:exe|com)$/i.test(cmd);
    child = spawn(cmd, runArgs, { cwd, env, stdio: ["inherit", "pipe", "pipe"], windowsHide: true, shell: useShell, detached: process.platform !== "win32" });
    try {
      if (child.pid && heavySlot) heavySlot.trackChild(child.pid, cmd);
    } catch (error) {
      child.once("error", () => {});
      if (child.pid) killTree(child.pid);
      child = null;
      reject(error);
      return;
    }
    const closeWatchers = watchProgress(watchDirs, () => { lastProgress = Date.now(); });
    // A killed step used to leave no trace of what it was doing, so every stall
    // cost a forensic dig. Keep a bounded tail to write out if we do kill it.
    let tail = "";
    for (const [stream, output] of [[child.stdout, process.stdout], [child.stderr, process.stderr]]) {
      stream.on("data", (chunk) => {
        lastProgress = Date.now();
        tail = `${tail}${chunk}`.slice(-64 * 1024);
        if (!managedRequestId) managedRequestId = firstManagedRequestId(tail);
        output.write(chunk);
      });
    }
    let lastCpu = processGroupCpuSeconds(child.pid);
    const monitor = setInterval(() => {
      const mtime = Math.max(...watchDirs.map(newestMtime));
      if (mtime > lastMtime) { lastMtime = mtime; lastProgress = Date.now(); }
      const cpu = processGroupCpuSeconds(child.pid);
      if (cpu !== null && lastCpu !== null && cpu > lastCpu) lastProgress = Date.now();
      if (cpu !== null) lastCpu = cpu;
      if (Date.now() - started > absoluteMs) void stop(`absolute limit exceeded (${Math.round(absoluteMs / 60000)}m)`, cpu);
      else if (Date.now() - lastProgress > inactivityMs) void stop(`no output, file, or CPU progress for ${Math.round(inactivityMs / 60000)}m`, cpu);
    }, Math.min(30_000, Math.max(250, Math.floor(inactivityMs / 4))));
    const cleanup = () => {
      clearInterval(monitor);
      closeWatchers();
      if (requestActiveRunStop === stop) requestActiveRunStop = null;
    };
    const stop = async (reason, cpu = null) => {
      if (stopping || settled) return;
      stopping = true;
      cleanup();
      let cancellation = { status: "NO_MANAGED_REQUEST" };
      try {
        cancellation = await cancelAndReapManagedRequest(managedRequestId, { cwd, env });
      } catch (error) {
        cancellation = { status: "CANCEL_REAP_FAILED", error: error.message };
      }
      if (diagnosticPath) {
        try {
          mkdirSync(path.dirname(diagnosticPath), { recursive: true });
          writeFileSync(diagnosticPath, [
            `reason: ${reason}`,
            `command: ${cmd} ${runArgs.join(" ")}`,
            `cwd: ${cwd}`,
            `watched: ${watchDirs.join(path.delimiter)}`,
            `elapsedMs: ${Date.now() - started}`,
            `cpuSeconds: ${cpu ?? "unavailable"}`,
            `managedRequestId: ${managedRequestId ?? "none"}`,
            `managedCancellation: ${JSON.stringify(cancellation)}`,
            `killedAt: ${new Date().toISOString()}`,
            "",
            "--- last output ---",
            tail || "(the step produced no output at all)",
            "",
          ].join("\n"));
          console.error(`right-release: stall diagnostic written to ${diagnosticPath}`);
        } catch { /* diagnostics are best effort; never mask the stall itself */ }
      }
      if (child?.pid) killTree(child.pid);
      child = null;
      settleReject(new Error(`release step stalled: ${reason}`));
    };
    requestActiveRunStop = stop;
    child.once("error", (error) => { cleanup(); child = null; if (!stopping) settleReject(error); });
    child.once("exit", (code) => { cleanup(); child = null; if (!stopping) code === 0 ? settleResolve() : settleReject(new Error(`${cmd} exited ${code}`)); });
  });
}

function newestMtime(dir) {
  if (!existsSync(dir)) return 0;
  try { return statSync(dir).mtimeMs; } catch { return 0; }
}

function checkpoint(stateRoot, name) {
  const file = path.join(stateRoot, "checkpoints.json");
  const state = existsSync(file) ? JSON.parse(readFileSync(file, "utf8")) : { schema: 1, completed: [] };
  if (!state.completed.includes(name)) state.completed.push(name);
  state.updatedAt = new Date().toISOString();
  writeJson(file, state);
}

function acquireLock(file) {
  if (existsSync(file)) {
    try {
      const existing = JSON.parse(readFileSync(file, "utf8"));
      if (!processAlive(existing.pid)) unlinkSync(file);
    } catch {
      unlinkSync(file);
    }
  }
  try {
    const fd = openSync(file, "wx");
    writeFileSync(fd, `${JSON.stringify({ pid: process.pid, startedAt: new Date().toISOString(), argv: process.argv })}\n`);
    closeSync(fd);
    return { release: () => { try { unlinkSync(file); } catch { /* already released */ } } };
  } catch (error) {
    if (error.code === "EEXIST") fail(`another ${platform} release is active: ${file}`);
    throw error;
  }
}

function processAlive(pid) {
  if (!Number.isInteger(Number(pid)) || Number(pid) <= 0) return false;
  try { process.kill(Number(pid), 0); return true; } catch { return false; }
}

function assertDiskSpace(dir, minimumGb) {
  const stats = statfsSync(dir);
  const freeGb = Number(stats.bavail * stats.bsize) / 1024 ** 3;
  if (freeGb < minimumGb) fail(`insufficient disk space: ${freeGb.toFixed(1)} GiB free, ${minimumGb} GiB required`);
}

function assertExecutables(names) {
  for (const name of names) if (!commandExists(name)) fail(`required executable not found: ${name}`);
}

function commandExists(name) {
  const probe = process.platform === "win32" ? spawnSync("where.exe", [name], { stdio: "ignore", windowsHide: true }) : spawnSync("sh", ["-lc", `command -v '${name.replaceAll("'", "")}'`], { stdio: "ignore" });
  return probe.status === 0;
}

function assertSccacheVersion(version) {
  const match = String(version ?? "").match(/sccache\s+(\d+)\.(\d+)\.(\d+)/i);
  if (!match || Number(match[1]) === 0 && Number(match[2]) < 15) fail("shared cache requires sccache >= 0.15.0; install with: cargo install sccache --locked; then verify: sccache --version");
}

function commandOutput(cmd, runArgs) {
  try {
    return commandOutputPortable(cmd, runArgs);
  } catch (error) {
    fail(`required command failed: ${error.message}`);
  }
}

function git(cwd, runArgs, { trim = true } = {}) {
  return commandOutputAt("git", runArgs, cwd, { trim });
}

function commandOutputAt(cmd, runArgs, cwd, { trim = true } = {}) {
  const result = spawnSync(cmd, runArgs, { cwd, encoding: "utf8", windowsHide: true });
  if (result.status !== 0) fail(`${cmd} ${runArgs.join(" ")} failed: ${(result.stderr ?? "").trim()}`);
  return trim ? result.stdout.trim() : result.stdout;
}

function runChecked(cmd, runArgs, cwd, env = process.env) {
  const result = spawnSync(cmd, runArgs, { cwd, env, stdio: "inherit", windowsHide: true, shell: false });
  if (result.status !== 0) fail(`${cmd} exited ${result.status}`);
}

function runConfiguredReleaseCheck(command, { root, env }) {
  if (!command) return;
  if (typeof command.cmd !== "string" || !command.cmd.trim()) {
    fail("release preSeal check must declare a command");
  }
  const args = Array.isArray(command.args) ? command.args.map(String) : [];
  const cwd = path.resolve(root, command.cwd ?? ".");
  const result = spawnSync(command.cmd, args, {
    cwd,
    env: { ...process.env, ...env },
    stdio: "inherit",
    windowsHide: true,
    shell: false,
  });
  if (result.status !== 0) fail(`release preSeal check failed: ${command.cmd} ${args.join(" ")}`);
}

function killTree(pid) {
  terminateProcessTree(pid);
}

function writeJson(file, value) {
  mkdirSync(path.dirname(file), { recursive: true });
  writeFileSync(file, `${JSON.stringify(value, null, 2)}\n`);
}

function usage(code) {
  console.log("usage: right-release build --platform win|mac|android|ios [--config right-release.config.mjs] [--dry-run]");
  process.exit(code);
}

function fail(message) {
  console.error(`right-release build: ${message}`);
  throw new Error(message);
}

function throwIfInterrupted() { if (interrupted) throw interrupted; }
