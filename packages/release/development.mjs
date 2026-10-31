import { discoverBrokerSocket, resolveManagedRoots } from './managed-roots.mjs';
import { spawnSync } from "./background-process.mjs";
import { createHash } from "node:crypto";
import { copyFileSync, cpSync, existsSync, mkdirSync, readFileSync, lstatSync, readdirSync, realpathSync, renameSync, rmSync, statSync } from "node:fs";
import { buildLaunch, createNativeRunner } from "./build-launch.mjs";
import { carriedOutputDir, carriedOutputDirsSince } from "./carried-cargo.mjs";
import { lockedTauriArgs } from "./tauri-locked.mjs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { defaultReleaseConfigFile, importReleaseConfig } from "./release-config.mjs";
import { assertPrimaryReleaseCheckout } from "./release-invocation.mjs";
import { readSourceSplit } from "./source-split.mjs";
import { assertReleaseExecutionAllowed } from "./execution-policy.mjs";
import { acquireManagedRunLease, directCargoEnv, directPlatform, managedRuntimeClient, managedRuntimeCommand, runCargoSync } from "./managed-cargo.mjs";
import { BUNDLE_MARKERS, bundleMarkerBuffer, readTauriBundleMarkers } from "./tauri-bundle-marker.mjs";
import { installPortableDevelopment } from "./portable-development.mjs";
import { previousPathOf, stagingPathOf, swapInPlace } from "./in-place-install.mjs";
import { stageDevelopmentBinaries, validateStageEntries } from "./stage-binary.mjs";
import { assertStableDesignatedRequirement, readBundleExecutable, requestedCiAdhocSigning, signMacosDevelopmentBundle } from "./development-signing.mjs";
import { macosCodesignEnvironment } from "./codesign-environment.mjs";
import { installTauriDevId, resolveTauriDevIdDescriptor, TAURI_DEVID_KIND } from "./install-tauri-devid.mjs";
import { TAURI_AUTHENTICODE_KIND, installTauriAuthenticode, resolveAuthenticodeDescriptor, validateAuthenticodeBuild } from "./install-tauri-authenticode.mjs";
import { signWindowsFiles, verifyWindowsFiles } from "./sign-windows.mjs";
import { assertSuiteSigningPolicy } from "./suite-signing-policy.mjs";
import { fsListRustFiles } from "./signing-files.mjs";
import { ASSEMBLED_APP_KIND, installAssembledMac, installAssembledWindows, resolveAssembledInstall } from "./install-assembled-app.mjs";
import { assertIosDevelopmentSession, installIosDevelopment, iosDevelopmentApp, iosUiTestCommands, prepareIosDevelopment, prepareIosUiTests, runIosDevelopmentCommands, validateIosDevelopment } from "./ios-dev.mjs";
import { packLinuxArtifacts, validateLinuxArchive } from "./linux-artifacts.mjs";
import { buildAndEmbedNativeHelpers, nativeHelpersFromConfig } from "./native-helpers.mjs";
import { androidDevelopmentNdkCommand, androidDevelopmentSerial, installAndroidDevelopment, validateAndroidDevelopment } from "./android-dev.mjs";

// One standard Cargo profile for every app (templates/cargo-profiles.toml): Cargo's built-in `dev`.
// `--profile dev` is valid and no flag also means dev.
export const DEVELOPMENT_PROFILE = "dev";
// Cargo writes the `dev` (and `test`) profile to target/<triple>/debug, never .../dev; `bench` goes to release.
// Every path built from a profile name must go through this.
export function cargoProfileDirectory(profile) {
  if (profile === "dev" || profile === "test") return "debug";
  if (profile === "bench") return "release";
  return profile;
}
const RELEASE_ONLY_APPLE_ENV = ["APPLE_ID", "APPLE_PASSWORD", "APPLE_API_KEY", "APPLE_API_ISSUER", "APPLE_API_KEY_PATH", "APPLE_CERTIFICATE", "APPLE_CERTIFICATE_PASSWORD"];

export function developmentEnvironment(env = process.env, platform = process.platform) {
  const result = {
    ...(directPlatform(platform) ? directCargoEnv(env, platform) : env),
    RIGHTKIT_BUILD_MODE: "dev",
    RIGHTKIT_CARGO_PROFILE: DEVELOPMENT_PROFILE,
    TAURI_SIGNING_PRIVATE_KEY: "",
    TAURI_SIGNING_PRIVATE_KEY_PASSWORD: "",
    CSC_IDENTITY_AUTO_DISCOVERY: "false",
  };
  for (const key of RELEASE_ONLY_APPLE_ENV) delete result[key];
  delete result.CARGO_INCREMENTAL;
  // A dev build is debug-signed: a release signing-properties path never reaches it.
  delete result.RIGHTKIT_ANDROID_SIGNING_PROPERTIES;
  // A direct (Linux) host never gets the managed agent-bin on PATH or a discovered socket, even when it DECLARES a broker route: managedRuntimeClient then returns the declared,
  // pinned client, but the agent-bin directory and socket here come from environment-derived managed roots that nothing validated. Whatever the build needs from the broker
  // goes through the executor's validated client (buildLaunch / createNativeRunner), never a PATH lookup, and a second endpoint variable would make the declared route ambiguous.
  const client = directPlatform(platform) ? undefined : managedRuntimeClient(result, platform);
  if (client) {
    const { agentBin } = resolveManagedRoots({ env: result, platform });
    if (agentBin && existsSync(agentBin)) {
      const pathKey = Object.keys(result).find((key) => key.toLowerCase() === "path") ?? "PATH";
      result[pathKey] = [agentBin, result[pathKey]].filter(Boolean).join(path.delimiter);
    }
    // Same default as the agent-bin `rightkit` shim: children that run the managed client directly
    // (resolveCommand("rustright"), build scripts) must not lose the broker endpoint.
    const socket = discoverBrokerSocket(result);
    if (!result.RUSTRIGHT_BUILD_BROKER_SOCKET && !result.RIGHTKIT_BUILD_BROKER_SOCKET && socket && existsSync(socket)) result.RUSTRIGHT_BUILD_BROKER_SOCKET = socket;
  }
  return result;
}

export function validateDevelopmentConfig(config, platform, { root = process.cwd() } = {}) {
  const target = config?.development?.targets?.[platform];
  // iOS builds its commands from project, scheme and install (ios-dev.mjs); there is no hand-written build array.
  if (platform === "ios") return validateIosDevelopment(target, { root });
  if (!target || !Array.isArray(target.build) || target.build.length === 0) throw new Error(`development.targets.${platform}.build must be a non-empty command array`);
  for (const [index, command] of target.build.entries()) validateCommand(command, `development.targets.${platform}.build[${index}]`);
  if (target.install?.kind === "command") validateCommand(target.install, `development.targets.${platform}.install`);
  else if (target.install?.kind === "tauri") {
    if (!target.install.productName || !target.install.targetTriple) throw new Error(`development.targets.${platform}.install requires productName and targetTriple`);
    if (target.install.manifestPath && path.isAbsolute(target.install.manifestPath)) throw new Error("install.manifestPath must be relative to app root");
  } else if (target.install?.kind === "portable") {
    const descriptor = target.install;
    const absoluteInstallRoot = platform === "win" ? path.win32.isAbsolute(descriptor.installRoot ?? "") : path.isAbsolute(descriptor.installRoot ?? "");
    if (descriptor.activation?.kind !== "current" || !absoluteInstallRoot) throw new Error("portable development installer requires absolute installRoot and current activation");
    for (const key of ["sourceDirectory", "entrypoint"]) {
      if (typeof descriptor[key] !== "string" || !descriptor[key] || path.isAbsolute(descriptor[key]) || path.win32.isAbsolute(descriptor[key]) || descriptor[key].split(/[\\/]/).includes("..")) throw new Error(`portable development ${key} must be a contained relative path`);
    }
  } else if (target.install?.kind === TAURI_DEVID_KIND) {
    if (platform !== "mac") throw new Error(`the ${TAURI_DEVID_KIND} development installer is for --platform mac`);
    resolveTauriDevIdDescriptor(target.install, { appRoot: root });
    if (!target.install.targetTriple && !target.install.bundleDirectory) throw new Error(`development.targets.${platform}.install requires targetTriple (or bundleDirectory) to find the built .app`);
  } else if (target.install?.kind === TAURI_AUTHENTICODE_KIND) {
    if (platform !== "win") throw new Error(`the ${TAURI_AUTHENTICODE_KIND} development installer is for --platform win`);
    resolveAuthenticodeDescriptor(target.install, `development.targets.${platform}.install`);
    validateAuthenticodeBuild(target.build, `development.targets.${platform}.build`, (rel) => { try { return readFileSync(path.resolve(root, rel), "utf8"); } catch { return null; } });
  } else if (target.install?.kind === ASSEMBLED_APP_KIND) {
    if (platform !== "mac" && platform !== "win") throw new Error(`the ${ASSEMBLED_APP_KIND} development installer is for --platform mac or win`);
    // The assembly (assembly.<platform>) and the install keys are validated together, before any build.
    resolveAssembledInstall({ config, platform });
  } else if (target.install?.kind === "adb") {
    if (platform !== "android") throw new Error("the adb development installer is for --platform android");
  } else if (target.install) throw new Error(`unsupported development installer kind: ${target.install.kind}`);
  // Linux is build-only: its artifacts are the staged sidecars bound in the stage receipt; nothing is installed on a Linux host.
  if (platform === "linux" && target.install) throw new Error("development.targets.linux is build-only: it takes no install (the staged artifacts are the result)");
  if (target.archive !== undefined && platform !== "linux") throw new Error(`development.targets.${platform}.archive: only the linux target packs an archive`);
  if (platform === "linux") validateLinuxArchive(target.archive);
  validateStageEntries(target.stage, `development.targets.${platform}.stage`);
  if (platform === "android") return { ...target, ...validateAndroidDevelopment(target, { root }) };
  return target;
}

export function runDevelopment({ root, platform, config, buildOnly = false, dryRun = false, env = process.env, run = spawnSync, door = buildLaunch,
  codesignEnvironment = macosCodesignEnvironment, serial, uiTests = false, hostPlatform = process.platform, nativeHost = process.platform }) {
  // UI test journeys (`right-release test --platform ios`): the declared XCUITest journeys on the dev device, under the same guards.
  if (uiTests && platform !== "ios") throw new Error("UI test journeys run for --platform ios only");
  if (!new Set(["win", "mac", "linux", "android", "ios"]).has(platform)) throw new Error("--platform must be win, mac, linux, android or ios");
  // iOS: --dry-run validates the config and prints the plan on any host; everything else needs macOS with Xcode.
  // `nativeHost` is the kernel the build runs on (assertNativeDevelopmentHost), `hostPlatform` the host the suite signing policy judges: the same on a real host, apart only in a fixture that
  // runs the Mac or Windows build mechanics on another kernel under a host the signing policy exempts.
  if (!(platform === "ios" && dryRun)) assertNativeDevelopmentHost(platform, nativeHost);
  const target = validateDevelopmentConfig(config, platform, { root });
  // One signing policy, enforced here whatever the config's format (the route probe reads JSON configs only; a legacy .mjs config is judged now): a dev
  // target that would build ad hoc, unsigned or under another identifier is refused before it builds (rightkit#1303). Mac and Dell only.
  if (!dryRun && !uiTests) assertSuiteSigningPolicy({ config, platform: hostPlatform, env, to: "dev", devPlatform: platform, buildOnly, listFiles: fsListRustFiles(root), readFile: (rel) => { try { return readFileSync(path.resolve(root, rel), "utf8"); } catch { return null; } } });
  // Inside the door's source split `root` is the snapshot of a commit (no git directory): the repository is the checkout the door
  // ran in, whose primary-checkout rule and execution policy still apply, and the commit is the split's.
  const split = readSourceSplit({ env, cwd: root });
  const repoRoot = split ? split.checkout : git(root, ["rev-parse", "--show-toplevel"]);
  assertPrimaryReleaseCheckout(repoRoot, env);
  if (dryRun && uiTests) {
    if (!target.uiTests) throw new Error("development.targets.ios.uiTests is not declared: list the journeys in uiTests.onlyTesting");
    return { built: false, installed: false, plan: { tests: iosUiTestCommands(target, { root, destinationId: "<device or simulator id>" }) } };
  }
  if (dryRun) return { built: false, installed: false, plan: { ...(target.ndk ? { ndk: androidDevelopmentNdkCommand(target.ndk) } : {}), build: target.build, install: buildOnly ? null : target.install ?? null, profile: DEVELOPMENT_PROFILE } };
  assertReleaseExecutionAllowed({ cwd: repoRoot, environment: env, command: `right-release dev --platform ${platform}` });
  // Refuse an invalid or off-CI ad hoc opt-in before the (long) build, not after it.
  if (!buildOnly && platform === "mac" && target.install?.kind === "tauri") requestedCiAdhocSigning({ descriptor: target.install, env });
  // iOS: the device or simulator is resolved (and a device proven ready) before the build, and a device build needs the GUI signing session.
  let iosPrepared = null;
  if (platform === "ios") {
    assertIosDevelopmentSession(target, { env, run });
    if (uiTests && !target.uiTests) throw new Error("development.targets.ios.uiTests is not declared: list the journeys in uiTests.onlyTesting");
    iosPrepared = uiTests ? prepareIosUiTests(target, { root, env, run, serial }) : prepareIosDevelopment(target, { root, env, run, serial });
  }
  // Android: the device serial is required, so a missing one is refused before the (long) build too.
  if (!buildOnly && platform === "android" && target.install) androidDevelopmentSerial({ descriptor: target.install, serial, env });
  const childEnv = codesignEnvironment(developmentEnvironment(env));
  // One lease spans every managed request this run makes, so a broker cutover cannot land between them.
  const lease = acquireManagedRunLease({ command: `right-release dev --platform ${platform}`, env });
  let releaseLease = true;
  try {
    const startedMs = Date.now();
    // Android: cargo-ndk (Cargo dev profile) fills the generated jniLibs, then the Gradle debug build packages it.
    if (target.ndk) { rmSync(target.ndk.jniLibs, { recursive: true, force: true }); execute(androidDevelopmentNdkCommand(target.ndk), { root, env: childEnv, dryRun, run, door, label: "native build" }); }
    // RA-09: the iOS compile steps (xcodegen, xcodebuild build / build-for-testing / test-without-building) are broker adapter jobs of the production native runner, planned together before the first
    // launch; there is no execute() of this file's own for them. The app's own non-native `prepare` commands are the only direct steps and keep going through the door. The built app and the
    // result bundle are read back from the verified receipt's managed output roles.
    const iosNative = (commands) => runIosDevelopmentCommands(commands, {
      native: createNativeRunner({ cwd: root, env: childEnv }), root, env: childEnv,
      direct: (command) => { execute(command, { root, env: childEnv, dryRun, run, door, label: "prepare" }); return { status: 0 }; },
    });
    if (uiTests) {
      const tested = iosNative(iosPrepared.tests);
      console.log(`right-release test: ios journeys passed (${target.uiTests.onlyTesting.join(", ")}); result bundle ${tested.resultBundle ?? "(not reported)"}`);
      return { built: true, installed: false, tested: true, resultBundle: tested.resultBundle };
    }
    let built = null;
    if (iosPrepared) built = iosNative(iosPrepared.build);
    else for (const command of target.build) execute(command, { root, env: childEnv, dryRun, run, door, label: "build" });
    if (iosPrepared?.install && !dryRun) iosPrepared.install.app = iosDevelopmentApp(target, built.outputs);
    // Dev and QA binaries run from one stable staged path, copied in place (no backup of the old copy), never from the
    // broker's per-generation target folder or a hashed dev-targets folder.
    const staged = target.stage?.length ? stageDevelopmentBinaries(target.stage, { root, platform: platform === "win" ? "win32" : process.platform, managedOutputDirs: carriedOutputDirsSince(childEnv) }) : [];
    for (const file of staged) console.log(`right-release dev: staged ${file}`);
    // Linux: the declared files become one tarball per architecture plus the digest manifest a guest link pins (rightkit#1230).
    // Only from a clean checkout: an artifact others install must name the exact commit it was built from.
    if (platform === "linux" && target.archive && !dryRun) {
      // A split's snapshot is exactly its commit (the door verifies it at the start and end of the stage); the checkout may hold anything.
      if (!split && git(repoRoot, ["status", "--porcelain", "--untracked-files=normal"])) throw new Error("linux artifacts are packed only from a clean checkout: commit first, so the manifest names the exact source");
      let version;
      try { version = JSON.parse(readFileSync(path.join(root, "package.json"), "utf8")).version; } catch { version = undefined; }
      packLinuxArtifacts({ root, archive: validateLinuxArchive(target.archive), app: target.archive.name, version, commit: split ? split.commit : git(repoRoot, ["rev-parse", "HEAD"]), run });
    }
    if (buildOnly || !target.install) return { built: true, installed: false, ...(staged.length ? { staged } : {}) };
    if (iosPrepared) return { built: true, installed: true, ios: installIosDevelopment(iosPrepared.install, { root, env: childEnv, run }) };
    if (target.install.kind === TAURI_DEVID_KIND) {
      // installTauriDevId is async: the lease is held until it settles, so a broker cutover cannot land mid-install.
      const built = resolveTauriArtifact(target.install, { root, platform, env: childEnv });
      // Dev = production + QA: the same native helpers the release embeds are built, embedded and signed into the dev .app too.
      const helpers = platform === "mac" ? nativeHelpersFromConfig(config) : undefined;
      if (helpers) {
        let version;
        try { version = JSON.parse(readFileSync(path.join(root, "package.json"), "utf8")).version; } catch { version = undefined; }
        buildAndEmbedNativeHelpers({ appRoot: root, app: built, helpers, version, forceArch: String(target.install.targetTriple).startsWith("x86_64") ? "x86_64" : "arm64", configuration: "debug", replaceExisting: true, env: childEnv });
      }
      releaseLease = false;
      return installTauriDevId({ appRoot: root, config: target.install, releaseConfig: config, built, env: childEnv })
        .then((outcome) => ({ built: true, installed: true, ...(staged.length ? { staged } : {}), devid: outcome }))
        .finally(() => lease.release());
    }
    if (target.install.kind === TAURI_AUTHENTICODE_KIND) {
      // The unsigned Windows installer is never run: this kind signs the exe, bundles it, signs the installer, then installs (install-tauri-authenticode.mjs).
      const descriptor = resolveAuthenticodeDescriptor(target.install, `development.targets.${platform}.install`);
      const bundleRoot = resolveBundleRoot(descriptor, { root, env: childEnv, runCargo: runCargoSync });
      const debugDir = path.resolve(bundleRoot, "..", "..");
      const bundleStartedMs = Date.now();
      const authenticode = installTauriAuthenticode({ descriptor, debugDir, appRoot: root, releaseConfig: config, deps: {
        signWindows: (files) => signWindowsFiles(files),
        verifyWindows: (files) => verifyWindowsFiles(files),
        bundle: (command) => execute(command, { root, env: childEnv, dryRun, run, door, label: "bundle" }),
        findInstaller: () => resolveTauriBundleDirectory(descriptor, { root, platform, env: childEnv, sinceMs: bundleStartedMs }).installer,
        install: (installer) => installWindowsDevelopment(descriptor, installer, { root, env: childEnv, run, debugDir }),
        installedExe: () => path.resolve(root, descriptor.expectedInstalledPath),
      } });
      return { built: true, installed: true, ...(staged.length ? { staged } : {}), authenticode };
    }
    if (target.install.kind === ASSEMBLED_APP_KIND) {
      const { spec, descriptor } = resolveAssembledInstall({ config, platform });
      if (platform === "win") {
        const assembled = installAssembledWindows({ root, config, descriptor, spec, env: childEnv });
        return { built: true, installed: true, ...(staged.length ? { staged } : {}), assembled };
      }
      // installAssembledMac is async: the lease is held until it settles, so a broker cutover cannot land mid-install.
      releaseLease = false;
      return installAssembledMac({ root, config, descriptor, spec, env: childEnv })
        .then((outcome) => ({ built: true, installed: true, ...(staged.length ? { staged } : {}), assembled: outcome }))
        .finally(() => lease.release());
    }
    const signing = install(target.install, { root, platform, config, env: childEnv, run, door, sinceMs: startedMs, serial });
    return { built: true, installed: true, ...(staged.length ? { staged } : {}), ...(platform === "mac" && target.install.kind === "tauri" ? { signing } : {}) };
  } finally { if (releaseLease) lease.release(); }
}

/** WSL reports `linux` as its platform under both generations; which one it is decides whether a Linux kernel runs at all. */
export function isWsl({ env = process.env, readFile = (f) => readFileSync(f, "utf8") } = {}) {
  if (env.WSL_DISTRO_NAME || env.WSL_INTEROP) return true;
  try { return /microsoft|wsl/i.test(readFile("/proc/sys/kernel/osrelease")) || /microsoft|wsl/i.test(readFile("/proc/version")); } catch { return false; }
}

/**
 * WSL1 translates Linux system calls onto the Windows kernel (osrelease like `4.4.0-19041-Microsoft`), so it is not a Linux host.
 * WSL2 runs a genuine Linux kernel in a Hyper-V utility VM (osrelease `…-microsoft-standard-WSL2`): a Linux VM, which is what
 * native Linux builds need (rightkit#1230: "x86_64 on a Linux VM on the Dell"; Astra 2026-10-28). Unknown WSL counts as WSL1.
 */
export function isWsl1({ env = process.env, readFile = (f) => readFileSync(f, "utf8") } = {}) {
  if (!isWsl({ env, readFile })) return false;
  try { return !/microsoft-standard-WSL2|WSL2/i.test(readFile("/proc/sys/kernel/osrelease")); } catch { return true; }
}

export function assertNativeDevelopmentHost(platform, host = process.platform, { wsl1 = () => isWsl1() } = {}) {
  // Linux builds run on a Linux kernel of the target's architecture (no cross-compiling); WSL1 has no Linux kernel.
  if ((platform === "win" && host !== "win32") || ((platform === "mac" || platform === "ios") && host !== "darwin") || (platform === "linux" && host !== "linux")) throw new Error(`development target ${platform} requires its native host`);
  if (platform === "linux" && wsl1()) throw new Error("development target linux requires a Linux kernel; this is WSL1 (Linux system calls translated onto the Windows kernel). Use WSL2 or a Linux VM");
}

function install(descriptor, { root, platform, config, env, run, door, sinceMs = null, serial }) {
  if (descriptor.kind === "adb") return installAndroidDevelopment(descriptor, { root, env, run, serial });
  if (descriptor.kind === "command") return execute(descriptor, { root, env, run, door, label: "install" });
  if (descriptor.kind === "portable") return installPortableDevelopment(descriptor, { root, platform: platform === "win" ? "win32" : "darwin", env });
  if (platform === "win") {
    if (!descriptor.expectedInstalledPath) throw new Error("Windows Tauri installer requires expectedInstalledPath for installed verification");
    const { bundleRoot, installer: app } = resolveTauriBundleDirectory(descriptor, { root, platform, env, sinceMs });
    return installWindowsDevelopment(descriptor, app, { root, env, run, debugDir: path.resolve(bundleRoot, "..", "..") });
  }
  return installMacosDevelopment(descriptor, { root, platform, config, env, run });
}

/** Run the silent NSIS installer of a Windows dev build in place and check the installed exe is this build's. */
function installWindowsDevelopment(descriptor, app, { root, env, run, debugDir }) {
  {
    const built = path.resolve(debugDir, path.basename(descriptor.expectedInstalledPath));
    assertNonEmptyExecutable(built);
    // Tauri's silent installer force-kills a running copy of the app (utils.nsh: IfSilent kill). When it
    // was running, pass /R so the installer relaunches it as the user (RunAsUser, outside this process tree).
    const running = isWindowsProcessRunning(path.basename(descriptor.expectedInstalledPath), { run });
    if (running) console.log(`right-release dev: ${descriptor.productName} is running; the installer will close it and relaunch it`);
    // The silent installer updates the fixed path in place. NO backup of the old copy (Orthic-Labs/rightkit#348): a stale
    // <exe>.previous left by an older RightKit is removed, nothing is copied, and a failed install is reinstalled from an older build.
    const installedBefore = path.resolve(root, descriptor.expectedInstalledPath);
    rmSync(previousPathOf(installedBefore), { recursive: true, force: true });
    const result = run(app, windowsInstallerArguments({ running }), { cwd: root, env, stdio: "inherit", windowsHide: true, shell: false });
    if (result.status !== 0) throw new Error(`development installer failed (${result.status}): ${app}`);
    const installed = path.resolve(root, descriptor.expectedInstalledPath);
    assertInstalledExecutableMatches(built, installed);
  }
}

function installMacosDevelopment(descriptor, { root, platform, config, env, run }) {
  const app = resolveTauriArtifact(descriptor, { root, platform, env });
  // One fixed install path per app: macOS keys privacy grants to it. No /Applications/<name> guess.
  if (!descriptor.destination) throw new DevelopmentInstallError("DEV_INSTALL_DESTINATION_REQUIRED", "development.targets.mac.install.destination is required: the one fixed path this app installs to (for example /Applications/<Product>.app)");
  const destination = path.resolve(descriptor.destination);
  mkdirSync(path.dirname(destination), { recursive: true });
  // Fixed staging name (not per-pid): the copy is signed and verified here, then renamed into place. Nothing runs from it.
  const staging = stagingPathOf(destination);
  rmSync(staging, { recursive: true, force: true });
  cpSync(app, staging, { recursive: true, verbatimSymlinks: true });
  let signing;
  let executableName;
  try {
    signing = signMacosDevelopmentBundle({ root, bundle: staging, descriptor, config, env, run });
    // Same identity as the installed copy, or fail: a changed designated requirement resets every privacy grant.
    if (!signing.adhoc && existsSync(destination)) assertStableDesignatedRequirement({ root, installed: destination, expected: signing.designatedRequirement, env, run });
    executableName = readBundleExecutable(staging, { root, env, run });
  } catch (error) { rmSync(staging, { recursive: true, force: true }); throw error; }
  swapInstalledMacosBundle({ staging, destination, executableName });
  if (signing.provisioningProfile) signing.provisioningProfile = path.join(destination, "Contents", "embedded.provisionprofile");
  if (signing.adhoc) console.log(`right-release dev: installed ${destination}; signing=ci-adhoc (ad hoc signed on a CI runner; NOT a release artifact); signature=adhoc; TeamIdentifier=none`);
  else console.log(`right-release dev: installed ${destination}; identity=${signing.identity}; TeamIdentifier=${signing.teamIdentifier}; profile=${signing.provisioningProfile ?? "none"}; designated=${signing.designatedRequirement}`);
  return signing;
}

export class DevelopmentInstallError extends Error {
  constructor(code, message) { super(message); this.name = "DevelopmentInstallError"; this.code = code; }
}

const NODE_FS = { existsSync, lstatSync, mkdirSync, copyFileSync, readFileSync, renameSync, rmSync, statSync };

/**
 * Move the signed staging bundle into place IN PLACE with NO backup (Orthic-Labs/rightkit#348: the old app is removed,
 * never kept as `<destination>.previous`; a stale `.previous` is removed too), and verify it: Info.plist exists and
 * the installed main executable (`executableName`, the staged CFBundleExecutable read through plutil by the caller) exists with the same SHA-256 as the staged one. Any failure removes the
 * new (absent or partial) install, does NOT restore the old app (reinstall from an older DMG), and throws a DevelopmentInstallError (code DEV_INSTALL_VERIFICATION_FAILED,
 * DEV_INSTALL_EXECUTABLE_MISSING or DEV_INSTALL_EXECUTABLE_MISMATCH). `fs` is the file-system seam (tests inject it).
 */
export function swapInstalledMacosBundle({ staging, destination, executableName, fs = NODE_FS }) {
  if (typeof executableName !== "string" || !executableName) throw new DevelopmentInstallError("DEV_INSTALL_VERIFICATION_FAILED", "swapInstalledMacosBundle requires the staged CFBundleExecutable name");
  const executable = path.join("Contents", "MacOS", executableName);
  const digest = (file) => createHash("sha256").update(fs.readFileSync(file)).digest("hex");
  let expected;
  try { expected = digest(path.join(staging, executable)); }
  catch (error) {
    fs.rmSync(staging, { recursive: true, force: true });
    throw new DevelopmentInstallError("DEV_INSTALL_EXECUTABLE_MISSING", `staged app has no main executable ${executable}: ${staging} (${error.message})`);
  }
  // The old app is removed and the staged one renamed over it; no old copy survives, even on failure.
  swapInPlace({
    staging, destination, previous: null, fs,
    verify: () => {
      if (!fs.existsSync(path.join(destination, "Contents", "Info.plist"))) throw new DevelopmentInstallError("DEV_INSTALL_VERIFICATION_FAILED", `installed app verification failed: ${destination}`);
      const installed = path.join(destination, executable);
      let actual;
      try { actual = digest(installed); }
      catch { throw new DevelopmentInstallError("DEV_INSTALL_EXECUTABLE_MISSING", `installed app verification failed: main executable ${executable} is missing in ${destination}`); }
      if (actual !== expected) throw new DevelopmentInstallError("DEV_INSTALL_EXECUTABLE_MISMATCH", `installed app verification failed: ${executable} SHA-256 ${actual} does not match the staged ${expected}`);
    },
  });
}

export function resolveTauriArtifact(descriptor, { root, platform, env = process.env, runCargo = runCargoSync }) {
  return resolveTauriBundleDirectory(descriptor, { root, platform, env, runCargo }).installer;
}

/**
 * GB-60 (GATE output pinning; E16/E19): a build whose compile is receipt-managed (a broker host's carried build) writes its outputs under the broker's
 * output directory a signed receipt named, never under the source tree or the snapshot. A configured path that reads "target/..." (relative to the Cargo
 * workspace) is therefore migrated EXPLICITLY onto that directory; a source-relative output path that is not under a `target` directory cannot hold what that
 * build wrote and is refused with a typed error. It is never silently ignored, never resolved to a stale same-name file in the snapshot, and never
 * replaced by a scan for the newest match. `declared` is the configured path relative to `root`; `manifestDir` the Cargo manifest's directory relative to it.
 */
export function receiptRelativeOutputPath({ declared, root, manifestDir = ".", managedOutputDir, label }) {
  const relative = path.relative(path.resolve(root), path.resolve(root, declared)).split(path.sep).join("/");
  const segments = relative.split("/").filter(Boolean);
  const manifestParts = String(manifestDir).split(/[\\/]/).filter((part) => part && part !== ".");
  const at = segments.findIndex((segment, index) => segment === "target" && segments.slice(0, index).every((part, i) => part === manifestParts[i]));
  if (at < 0) {
    const error = new Error(`${label} ${JSON.stringify(declared)} is a source-relative path, but this build is receipt-managed: its outputs are under ${managedOutputDir}, not the source tree or the snapshot. Declare it relative to the Cargo target directory (target/<triple>/debug/bundle/...), or omit it so the receipt's directory is used`);
    error.code = "DEV_OUTPUT_SOURCE_RELATIVE";
    throw error;
  }
  return path.join(managedOutputDir, ...segments.slice(at + 1));
}

export function resolveTauriBundleDirectory(descriptor, { root, platform, env = process.env, runCargo = runCargoSync, sinceMs = null }) {
  let bundleRoot;
  if (descriptor.bundleDirectory) {
    const declared = path.resolve(root, descriptor.bundleDirectory);
    if (!inside(root, declared)) throw new Error("Tauri bundleDirectory must stay inside app root");
    const managed = descriptor.manifestPath ? carriedOutputDir(env, path.resolve(root, descriptor.manifestPath), { preferNested: true }) : null;
    bundleRoot = managed
      ? receiptRelativeOutputPath({ declared: descriptor.bundleDirectory, root, manifestDir: path.dirname(descriptor.manifestPath), managedOutputDir: managed, label: "Tauri bundleDirectory" })
      : declared;
  } else bundleRoot = resolveBundleRoot(descriptor, { root, env, runCargo });
  const entries = readdirSync(bundleRoot, { withFileTypes: true });
  const candidates = platform === "mac"
    ? entries.filter((entry) => entry.isDirectory() && entry.name === `${descriptor.productName}.app`).map((entry) => path.join(bundleRoot, entry.name))
    // Older installers are kept for history: select this product's installer written by this run.
    : entries.filter((entry) => entry.isFile() && entry.name.toLowerCase().endsWith(".exe") && entry.name.toLowerCase().includes("setup") && entry.name.startsWith(`${descriptor.productName}_`))
      .map((entry) => path.join(bundleRoot, entry.name))
      .filter((file) => sinceMs === null || statSync(file).mtimeMs >= sinceMs);
  if (candidates.length !== 1) throw new Error(`expected exactly one ${platform} Tauri installer for ${descriptor.productName} in ${bundleRoot}${sinceMs === null ? "" : " written by this run"}; found ${candidates.length}`);
  if (!statSync(candidates[0]).isFile() && platform === "win") throw new Error(`installer is not a file: ${candidates[0]}`);
  return { bundleRoot, installer: candidates[0] };
}

export function windowsInstallerArguments({ running = false } = {}) { return running ? ["/S", "/R"] : ["/S"]; }

export function isWindowsProcessRunning(imageName, { run = spawnSync, systemRoot = process.env.SystemRoot ?? "C:\\Windows" } = {}) {
  const result = run(path.join(systemRoot, "System32", "tasklist.exe"), ["/FI", `IMAGENAME eq ${imageName}`, "/FO", "CSV", "/NH"], { encoding: "utf8", windowsHide: true, shell: false });
  if (result.status !== 0 || typeof result.stdout !== "string") return false;
  return result.stdout.toLowerCase().includes(`"${imageName.toLowerCase()}"`);
}

export function assertInstalledExecutableMatches(built, installed, read = readFileSync) {
  assertNonEmptyExecutable(built);
  assertNonEmptyExecutable(installed);
  const digest = (file) => createHash("sha256").update(withUnknownBundleMarker(file, read)).digest("hex");
  if (digest(built) !== digest(installed)) throw new Error(`installed executable does not match this dev build: ${installed}`);
}
// Tauri's NSIS bundler rewrites the packaged copy's bundle marker UNK -> NSS; compare both copies as UNK.
function withUnknownBundleMarker(file, read) {
  const bytes = Buffer.from(read(file));
  const nsis = readTauriBundleMarkers(file, { read: () => bytes }).markers.nsis ?? [];
  if (nsis.length > 1) throw new Error(`installed executable verification failed: ${nsis.length} NSIS bundle markers in ${file}`);
  if (nsis.length === 1) bundleMarkerBuffer(BUNDLE_MARKERS.unknown).copy(bytes, nsis[0]);
  return bytes;
}
function assertNonEmptyExecutable(file) { if (!existsSync(file) || !statSync(file).isFile() || statSync(file).size === 0) throw new Error(`installed executable verification failed: missing or empty file ${file}`); }

function resolveBundleRoot(descriptor, { root, env, runCargo }) {
  if (!descriptor.manifestPath) throw new Error("Tauri installer requires manifestPath when bundleDirectory is omitted");
  const manifest = path.resolve(root, descriptor.manifestPath);
  // The bundle root is under the output directory a signed receipt of this stage named, not under a read-only metadata call
  // (the compile the Tauri CLI made nested under the build step's carried adapter job, from the broker's signed list of its children).
  const fromReceipt = carriedOutputDir(env, manifest, { preferNested: true });
  if (fromReceipt) return path.join(fromReceipt, descriptor.targetTriple, cargoProfileDirectory(DEVELOPMENT_PROFILE), "bundle", platformBundleKind(descriptor));
  const cwd = process.cwd();
  let metadata;
  try {
    process.chdir(root);
    metadata = runCargo(["metadata", "--offline", "--no-deps", "--format-version", "1", "--manifest-path", manifest], { cwd: root, env, encoding: "utf8", windowsHide: true, shell: false });
  } finally { process.chdir(cwd); }
  if (metadata.error || metadata.status !== 0) throw new Error(`RightKit Cargo metadata failed (${metadata.status ?? "unavailable"})${metadata.error ? `: ${metadata.error.message}` : ""}`);
  let targetDirectory;
  // A carried call (a snapshot stage's, carried-cargo.mjs) reports the signed receipt's output directory, not what metadata prints.
  if (typeof metadata.outputDir === "string") targetDirectory = metadata.outputDir;
  else try { targetDirectory = JSON.parse(metadata.stdout).target_directory; } catch { throw new Error("RightKit Cargo metadata returned invalid JSON"); }
  if (!targetDirectory || !path.isAbsolute(targetDirectory)) throw new Error("Cargo metadata omitted absolute target_directory");
  return path.join(targetDirectory, descriptor.targetTriple, cargoProfileDirectory(DEVELOPMENT_PROFILE), "bundle", platformBundleKind(descriptor));
}

function platformBundleKind(descriptor) { return descriptor.targetTriple.includes("apple") ? "macos" : "nsis"; }
function validateCommand(command, label) { if (!command || typeof command.cmd !== "string" || !command.cmd.trim() || (command.args !== undefined && !Array.isArray(command.args))) throw new Error(`${label} must declare cmd and args`); }
function execute(command, { root, env, dryRun, run, label, door = null }) {
  validateCommand(command, `development ${label}`);
  const cwd = path.resolve(root, command.cwd ?? ".");
  if (!inside(root, cwd)) throw new Error(`development ${label} cwd must stay inside app root`);
  const args = lockedTauriArgs((command.args ?? []).map(String), env);
  if (dryRun) return { cmd: command.cmd, args, cwd };
  // A pnpm step through the door is the broker's rightkit-pnpm job (or plain pnpm on a direct host), launched by name: the door never uses
  // the node entry resolveCommand would find, so resolving pnpm's JavaScript CLI here would only fail a host that does not need it.
  // The door's own `pnpm exec right-release` forms still resolve to this package's right-release.
  const ownReleaseStep = (args[0] === "exec" && args[1] === "right-release") || args[0] === "right-release";
  const resolved = door && command.cmd === "pnpm" && !ownReleaseStep ? { command: "pnpm", args } : resolveCommand(command.cmd, args, { root, env });
  // Every dev build step goes through the one door (build-launch.mjs): the door's own right-release stays in process order, a pnpm step
  // is the rightkit-pnpm broker job (by its own name, not the node entry resolveCommand finds), anything else as resolved.
  // A configured command can run code regardless of its label, so installation commands use the door too.
  const own = resolved.command === process.execPath && resolved.args[0] === OWN_RIGHT_RELEASE;
  const launch = own || !door ? resolved : { ...resolved, ...door(command.cmd === "pnpm" ? "pnpm" : resolved.command, command.cmd === "pnpm" ? args : resolved.args, { env: resolved.env ?? env, stepEnv: command.env, cwd, exists: existsSync }) };
  const result = run(launch.command, launch.args, { cwd, env: launch.env && launch.env !== resolved.env ? { ...(resolved.env ?? env), ...launch.env } : (launch.env ?? env), stdio: "inherit", windowsHide: true, shell: false, timeout: command.timeoutMs });
  if (result.status !== 0) throw new Error(`development ${label} command failed (${result.status}): ${command.name ?? command.cmd}`);
}
/** This package's own right-release entry: the door's RightKit, whatever the app's node_modules holds. */
const OWN_RIGHT_RELEASE = fileURLToPath(new URL("./cli/right-release.mjs", import.meta.url));
/** `rustright <tool>` forms that map to the real toolchain on a direct (Linux) host. */
const DIRECT_TOOLS = new Set(["cargo", "rustc", "rustdoc"]);
export function resolveCommand(command, args, { root, env = process.env, platform = process.platform } = {}) {
  if (command === "node") return { command: process.execPath, args };
  // A dev target's `right-release …` step runs the door's own right-release (this package), never the app's node_modules copy:
  // `pnpm exec right-release` puts the app's bin first on PATH, so PATH order cannot decide it (HeardRight on the Dell, 2026-10-27:
  // the app's older guard refused what the door had admitted). `pnpm exec right-release …` and `pnpm right-release …` are the same step.
  if (command === "right-release") return { command: process.execPath, args: [OWN_RIGHT_RELEASE, ...args] };
  if (command === "pnpm" && ((args[0] === "exec" && args[1] === "right-release") || args[0] === "right-release")) {
    return { command: process.execPath, args: [OWN_RIGHT_RELEASE, ...args.slice(args[0] === "exec" ? 2 : 1)] };
  }
  if (command === "pnpm") return { command: process.execPath, args: [resolvePnpmCli({ root, env }), ...args] };
  if (command === "rightkit") throw new Error("`rightkit` is the retired name of the RustRight client (rustright#91); use rustright (cmd: \"rustright\")");
  // A direct (Linux) host has no broker: `rustright cargo <args>` runs the real cargo, RustRight shims off PATH (managed-cargo directPlatform).
  if (command === "rustright" && directPlatform(platform)) {
    if (!DIRECT_TOOLS.has(args[0])) throw new Error(`\`rustright ${args[0] ?? ""}\` has no direct form on this ${platform} host (no broker on Linux); only \`rustright cargo|rustc|rustdoc\` map to the toolchain`);
    return { command: args[0], args: args.slice(1), env: directCargoEnv(env, platform) };
  }
  if (command === "rustright") {
    const client = managedRuntimeClient(env);
    if (!client) throw new Error("installed RightKit runtime client is unavailable for development command");
    const socket = discoverBrokerSocket(env);
    const childEnv = !env.RUSTRIGHT_BUILD_BROKER_SOCKET && !env.RIGHTKIT_BUILD_BROKER_SOCKET && existsSync(socket) ? { ...env, RUSTRIGHT_BUILD_BROKER_SOCKET: socket } : undefined;
    return { ...managedRuntimeCommand(client, args), ...(childEnv ? { env: childEnv } : {}) };
  }
  return { command, args };
}
export function resolvePnpmCli({ root, env = process.env } = {}) {
  const candidates = [];
  if (env.npm_execpath && /pnpm/i.test(path.basename(env.npm_execpath))) candidates.push(env.npm_execpath);
  const nodeDir = path.dirname(process.execPath);
  candidates.push(path.join(nodeDir, "node_modules", "corepack", "dist", "pnpm.js"));
  candidates.push(path.join(nodeDir, "node_modules", "pnpm", "bin", "pnpm.cjs"));
  for (let dir = path.resolve(root ?? process.cwd()); ; dir = path.dirname(dir)) {
    const store = path.join(dir, "node_modules", ".pnpm");
    if (existsSync(store)) {
      for (const entry of readdirSync(store).filter((name) => /^pnpm@/.test(name)).sort().reverse()) {
        candidates.push(path.join(store, entry, "node_modules", "pnpm", "bin", "pnpm.cjs"));
      }
    }
    const parent = path.dirname(dir);
    if (parent === dir) break;
  }
  // A global pnpm (no Corepack in Node >= 25) exposes its JS entry only through its PATH shim.
  for (const dir of String(env.PATH ?? env.Path ?? "").split(path.delimiter).filter(Boolean)) {
    const shim = path.join(dir, "pnpm.CMD");
    if (!existsSync(shim)) continue;
    const match = /"%~dp0\\([^"]*pnpm\.(?:mjs|cjs|js))"/i.exec(readFileSync(shim, "utf8"));
    if (match) candidates.push(path.resolve(dir, match[1]));
  }
  // Unix global pnpm exposes its JavaScript entry through a PATH symlink.
  for (const dir of String(env.PATH ?? env.Path ?? "").split(path.delimiter).filter(Boolean)) candidates.push(path.join(dir, "pnpm"));
  const selected = candidates.map(resolveJavaScriptEntrypoint).find(Boolean);
  if (!selected) throw new Error("could not resolve pnpm JavaScript CLI entrypoint; install/pin pnpm through Corepack");
  return selected;
}
function resolveJavaScriptEntrypoint(candidate) {
  try {
    const resolved = realpathSync(candidate);
    return /pnpm\.(?:mjs|cjs|js)$/i.test(resolved) && statSync(resolved).isFile() ? resolved : undefined;
  } catch {
    return undefined;
  }
}
function git(cwd, args) { const result = spawnSync("git", args, { cwd, encoding: "utf8", windowsHide: true }); if (result.status !== 0) throw new Error(`git ${args.join(" ")} failed`); return result.stdout.trim(); }
function inside(root, child) { const rel = path.relative(path.resolve(root), path.resolve(child)); return rel === "" || (rel !== ".." && !rel.startsWith(`..${path.sep}`) && !path.isAbsolute(rel)); }
export async function main(args = process.argv.slice(2)) {
  let platform, configPath = defaultReleaseConfigFile(), dryRun = false, buildOnly = false, serial;
  for (let i = 0; i < args.length; i++) {
    if (args[i] === "-h" || args[i] === "--help") { console.log("usage: right-release dev --platform win|mac|linux|android|ios [--config right-release.config.json] [--dry-run] [--build-only] [--serial <adb serial | iOS device id>]"); return; }
    if (args[i] === "--platform") platform = args[++i];
    else if (args[i] === "--config") configPath = args[++i];
    else if (args[i] === "--dry-run") dryRun = true;
    else if (args[i] === "--build-only") buildOnly = true;
    else if (args[i] === "--serial") serial = args[++i];
    else throw new Error(`unknown dev option: ${args[i]}`);
  }
  if (!platform) throw new Error("right-release dev requires --platform win|mac|linux|android|ios");
  const absoluteConfig = path.resolve(configPath);
  const safeEnv = developmentEnvironment(process.env);
  for (const [key, value] of Object.entries(safeEnv)) process.env[key] = value;
  for (const key of RELEASE_ONLY_APPLE_ENV) delete process.env[key];
  delete process.env.CARGO_INCREMENTAL;
  const pathKey = Object.keys(process.env).find((key) => key.toLowerCase() === "path");
  if (pathKey) process.env[pathKey] = safeEnv[pathKey] ?? process.env[pathKey];
  const config = await importReleaseConfig(absoluteConfig, { lenient: true });
  const result = await runDevelopment({ root: path.dirname(absoluteConfig), platform, config, dryRun, buildOnly, serial });
  if (dryRun) console.log(JSON.stringify(result.plan, null, 2));
  return result;
}

const realOrSelf = (file) => { try { return realpathSync(file); } catch { return path.resolve(file); } };
if (process.argv[1] && realOrSelf(process.argv[1]) === realOrSelf(fileURLToPath(import.meta.url))) {
  main().catch((error) => { console.error(`right-release dev: ${error.code ? `[${error.code}] ` : ""}${error.message}`); process.exitCode = 1; });
}
