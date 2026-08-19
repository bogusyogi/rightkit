#!/usr/bin/env node
import { createHash } from "node:crypto";
import {
  chmodSync,
  closeSync,
  copyFileSync,
  existsSync,
  mkdtempSync,
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
import { tmpdir } from "node:os";
import path from "node:path";
import { spawn, spawnSync } from "node:child_process";
import { fileURLToPath, pathToFileURL } from "node:url";
import { assertPrimaryReleaseCheckout, dirtyBuildInputs, resolveReleaseBuildInputs } from "./release-invocation.mjs";
import { commandOutputPortable, releaseEnvironment, resolveReleaseLayout, runBuildStateMachine, verifySealedRelease, watchProgress } from "./release-state.mjs";
import { acquireCacheLease, acquireSuiteBuildSlot, applyCachePrune, assertWriteVolumeFloors, inspectCache, inspectWriteVolumes, markCacheEntrySuccessful, planCachePrune, readCachePolicy, resolveCacheLayout, resolveSharedCacheIdentity, resolveSharedCacheRoot } from "./cache-policy.mjs";
import { createTargetBridge } from "./target-bridge.mjs";
import { brokerManagedHost } from "./cargo-contract.mjs";
import { resolveTargetRoot } from "./cargo-target.mjs";
import { assertNsisInPlaceUpgradeContract } from "./nsis-upgrade-contract.mjs";
import { buildPathPrefix, collectPreflight, formatPreflight, preflightFailures } from "./preflight.mjs";
import { assertCleanSource } from "./source-gate.mjs";
import { acquireHeavyWorkSlot, heavyCommandEnvironment, terminateProcessTree } from "./heavy-command.mjs";
import { processGroupCpuSeconds } from "./process-liveness.mjs";

// Fingerprint of the pipeline code that can change the bytes we ship or the way
// they are signed — deliberately NOT the package version, which moves for docs
// and test-only edits and would force a cold Rust rebuild of every app each time.
// These four files are the ones whose behaviour the cached target directory can
// outlive.

const PIPELINE_FINGERPRINT_SOURCES = ["release.mjs", "sign-windows.mjs", "tauri-bundle-marker.mjs", "nsis-payload.mjs"];
const PIPELINE_FINGERPRINT = createHash("sha256")
  .update(PIPELINE_FINGERPRINT_SOURCES.map((file) => `${file}:${createHash("sha256").update(readFileSync(path.join(path.dirname(fileURLToPath(import.meta.url)), file))).digest("hex")}`).join("\n"))
  .digest("hex")
  .slice(0, 16);

/** Assemble preflight inputs from the app's own files (mirrors release.mjs). */
function buildPreflight({ config, configPath, appRoot, repoRoot, platform }) {
  let version;
  try {
    version = JSON.parse(readFileSync(path.join(appRoot, "package.json"), "utf8")).version;
  } catch {
    version = undefined;
  }
  return collectPreflight({
    platform,
    appRoot,
    repoRoot,
    app: config.app,
    configPath,
    // The version check is the doctor's job: by the time `build` runs, seal
    // collision is already enforced downstream, and duplicating it here would
    // reject a legitimate resume of an in-flight build.
    version: undefined,
    cargoLockPaths: [
      path.join(appRoot, "src-tauri", "Cargo.lock"),
      path.join(appRoot, "Cargo.lock"),
    ],
  });
}

const TOOL_ROOT = path.dirname(fileURLToPath(import.meta.url));
const WORKER = path.join(TOOL_ROOT, "release.mjs");
const args = process.argv.slice(2);
let configName = "right-release.config.mjs";
let platform = process.platform === "win32" ? "win" : process.platform === "darwin" ? "mac" : process.platform;
let dryRun = false;

for (let i = 0; i < args.length; i += 1) {
  const arg = args[i];
  if (arg === "--config") configName = args[++i];
  else if (arg === "--platform") platform = args[++i];
  else if (arg === "--dry-run") dryRun = true;
  else if (arg === "--tier" || arg.startsWith("--tier=")) fail("build is tier-neutral; select patch or update only during upload");
  else if (arg === "-h" || arg === "--help") usage(0);
  else fail(`unknown argument: ${arg}`);
}
if (platform !== "win" && platform !== "mac") fail("--platform must be win or mac");

const configPath = path.resolve(configName);
const invocationRoot = path.dirname(configPath);
const repoRoot = git(invocationRoot, ["rev-parse", "--show-toplevel"]);
assertPrimaryReleaseCheckout(repoRoot);
const relativeConfig = path.relative(repoRoot, configPath).replaceAll("\\", "/");
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
  if (!target?.package) fail(`${config.app} has no ${platform} package command`);
  console.log(`right-release build dry-run: ${config.app} ${config.version} ${platform}`);
  console.log(`config: ${configPath}`);
  console.log(`commit: ${commit}`);
  process.exit(0);
}

for (const signal of ["SIGINT", "SIGTERM", "SIGHUP"]) {
  process.once(signal, () => {
    if (child?.pid) killTree(child.pid);
    interrupted = new Error(`release interrupted by ${signal}`);
    process.exitCode = signal === "SIGINT" ? 130 : 143;
  });
}

try {
  throwIfInterrupted();
  const commit = git(repoRoot, ["rev-parse", "HEAD"]);
  assertCleanSource({
    status: git(repoRoot, ["status", "--porcelain=v1", "-z", "--untracked-files=all"]),
    commandId: "right-release build",
  });
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
  if (!target?.package) fail(`${config.app} has no ${platform} package command`);
  if (platform === "win") assertNsisInPlaceUpgradeContract(appRoot, target.nsisUpgradeContract);
  const { requiredInputs, buildInputs } = resolveReleaseBuildInputs({ repoRoot, appRoot, configPath, config, target });
  const dirtyInputs = dirtyBuildInputs(repoRoot, buildInputs, commit);
  if (dirtyInputs.length > 0) fail(`dirty files can change the packaged app; resolve them before release:\n${dirtyInputs.map((file) => `- ${file}`).join("\n")}`);
  const toolVersions = collectToolVersions(config.packageManager ?? "pnpm");
  const inputHashes = hashInputs(requiredInputs);
  const receiptRoot = path.resolve(process.env.RIGHT_RELEASE_STATE_ROOT ?? path.join(appRoot, ".right-release", "receipts"));
  const signingIdentity = platform === "win" ? {
    contract: target.signingContract ?? "<missing>",
    // Pipeline code participates in cache identity. Without it, a fix to the
    // signing or packaging logic leaves the previous logic's target directory
    // eligible for reuse — which is how outputs from a known-bad pipeline
    // outlived the change that was supposed to retire them.
    pipeline: PIPELINE_FINGERPRINT,
    configSha256: hashFile(configPath),
    receiptInputs: ["raw-exe", "installer", "embedding"].map((phase) => path.join(receiptRoot, `windows-${phase}.json`)),
  } : null;
  if (signingIdentity) inputHashes[".right-release/signing-identity.json"] = hashFileText(JSON.stringify(signingIdentity));
  const cargoLock = requiredInputs.find((file) => /Cargo\.lock$/i.test(file));
  const cargoToml = requiredInputs.find((file) => /Cargo\.toml$/i.test(file));
  // A broker-managed host is not identified by RIGHTKIT_BUILD_BROKER_SOCKET alone:
  // that variable is exported by login shells only, while the managed cargo shim is
  // on PATH for every shell. Detecting on the variable alone made agent (non-login)
  // shells take the Cache V2 branch, bridge src-tauri/target into the release cache,
  // and then lose CARGO_TARGET_DIR to the broker's unconditional override — so the
  // app bundle landed in the broker workspace and packaging failed on an empty cache.
  const managedCargoTarget = brokerManagedHost(process.env) ? resolveTargetRoot(cargoToml) : null;
  const cacheIdentity = resolveSharedCacheIdentity({ cargoLockPath: cargoLock, cargoTomlPath: cargoToml, rustcVerbose: toolVersions.rustc, targetTriple: target.cargoTarget ?? target.targetTriple ?? target.rustTarget ?? toolVersions.rustHost, profile: target.profile ?? "release" });
  const cacheKey = hashFileText(JSON.stringify({ cache: cacheIdentity.fingerprint, signingIdentity })).slice(0, 16);
  const cacheModeRequested = process.env.RIGHT_RELEASE_CACHE_MODE ?? (platform === "mac" || platform === "win" ? "shared" : "legacy");
  if (cacheModeRequested !== "legacy" && cacheModeRequested !== "shared") fail("RIGHT_RELEASE_CACHE_MODE must be legacy or shared");
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
  const preflight = buildPreflight({ config, configPath, appRoot, repoRoot: layout.repoRoot, platform });
  const preflightBlockers = preflightFailures(preflight);
  if (preflightBlockers.length > 0) {
    fail(`preflight found ${preflightBlockers.length} blocking problem(s):\n${formatPreflight(preflightBlockers)}`);
  }
  for (const check of preflight) {
    if (check.status !== "ok") console.error(`[right-release] preflight ${check.name}: ${check.detail}`);
  }

  // A tool being INSTALLED is not the same as it being the one PATH picks. The
  // vendored-OpenSSL build shells out to `perl`, and under Git Bash/MSYS that
  // resolves to MSYS Perl, which cannot configure OpenSSL. Put the real Windows
  // Perl first so the build behaves the same from any shell.
  const pathPrefix = buildPathPrefix({ platform });
  const env = {
    ...heavyCommandEnvironment(process.env),
    ...releaseEnvironment({ root: vaultRoot, cacheRoot: sharedCacheRoot, platform, architecture: cacheIdentity.architecture, app: config.app, cacheKey, kind: "release", appRoot, mode: cacheMode }),
    ...(pathPrefix ? { PATH: `${pathPrefix}${path.delimiter}${process.env.PATH ?? ""}` } : {}),
    RIGHT_RELEASE_REPO_ROOT: layout.repoRoot,
    RIGHT_RELEASE_APP_ROOT: layout.appRoot,
  };
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
  throwIfInterrupted();
  const releaseId = `${config.app}-${config.version}-${shortCommit}`;
  const platformDir = platform === "win" ? "windows" : "mac";
  const buildRoot = managedCargoTarget ? null : path.join(vaultRoot, "build", config.app, config.version, shortCommit, platformDir);
  const stateRoot = path.join(vaultRoot, "state", releaseId, platformDir);
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
  const targetLink = path.join(appRoot, "src-tauri", "target");
  // ownedRoot is the per-app cache parent holding every fingerprint dir, so a
  // link left by an earlier fingerprint is recognised as ours and repointed
  // instead of hard-stopping the build.
  const targetBridge = createTargetBridge({
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
        assertExecutables(["git", config.packageManager ?? "pnpm", "cargo", "rustc", ...(target.preflight?.executables ?? [])]);
        for (const name of target.preflight?.env ?? []) if (!process.env[name]) fail(`missing required environment variable: ${name}`);
        for (const command of target.preflight?.commands ?? []) runChecked(command.cmd, command.args ?? [], path.resolve(appRoot, command.cwd ?? "."), env);
        if (buildRoot) mkdirSync(buildRoot, { recursive: true });
        mkdirSync(stateRoot, { recursive: true });
        if (buildRoot) writeJson(path.join(buildRoot, "release-inputs.lock.json"), lockPayload);
        writeJson(path.join(stateRoot, "release-inputs.lock.json"), lockPayload);
        checkpoint(stateRoot, "preflight_complete");
      },
      prepare: async () => {
        throwIfInterrupted();
        const cacheTarget = managedCargoTarget ?? env.CARGO_TARGET_DIR;
        mkdirSync(cacheTarget, { recursive: true });
        // Broker-managed hosts route through the same target bridge as shared
        // mode even though cacheMode is forced to "broker" above: the naive
        // else-branch below fail()s outright when targetLink is a real
        // directory, which is exactly the "corruption" misreading that nearly
        // cost ~5.9 GB of warm broker intermediates. The bridge's brokerManaged
        // flag already knows to leave a real directory untouched instead.
        if (cacheMode === "shared" || managedCargoTarget) targetBridge.ensure();
        else if (!existsSync(targetLink)) symlinkSync(cacheTarget, targetLink, process.platform === "win32" ? "junction" : "dir");
        else if (realpathSync(targetLink) !== realpathSync(cacheTarget)) {
          fail(`primary checkout target exists and is not the release cache link: ${targetLink}`);
        }
        await runProgress(config.packageManager ?? "pnpm", ["install", "--frozen-lockfile"], appRoot, env, cacheTarget, path.join(stateRoot, "stall-install.log"));
      },
      build: async () => {
        throwIfInterrupted();
        await runProgress(
          process.execPath,
          [WORKER, "--config", configPath, "--platform", platform, "--no-upload"],
          appRoot,
          env,
          [managedCargoTarget ?? env.CARGO_TARGET_DIR, path.join(appRoot, ".cache")],
          path.join(stateRoot, "stall-build.log"),
        );
        checkpoint(stateRoot, "build_complete");
        checkpoint(stateRoot, "signed");
        checkpoint(stateRoot, "hardened");
      },
      seal: async ({ sealedDir }) => {
        sealRelease({ configRoot: appRoot, sealedDir, releaseId, config, target, platform, commit, inputHashes, toolVersions, cacheKey, signingIdentity });
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
  return Object.fromEntries(files.map((file) => [path.relative(path.dirname(path.dirname(file)), file), hashFile(file)]));
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

function sealRelease({ configRoot, sealedDir, releaseId, config, target, platform, commit, inputHashes, toolVersions, cacheKey, signingIdentity }) {
  const sources = new Map();
  const addSource = (source, role) => {
    const current = sources.get(source) ?? new Set();
    current.add(role);
    sources.set(source, current);
  };
  for (const artifact of target.installer?.artifacts ?? []) addSource(path.resolve(configRoot, artifact.file), "installer");
  for (const artifact of target.updater?.artifacts ?? []) {
    addSource(path.resolve(configRoot, artifact.file), "updater");
    addSource(path.resolve(configRoot, artifact.signature), "updater-signature");
  }
  for (const file of sources.keys()) if (!existsSync(file)) fail(`cannot seal missing artifact: ${file}`);
  const temp = `${sealedDir}.tmp-${process.pid}`;
  if (existsSync(temp)) rmSync(temp, { recursive: true, force: true });
  mkdirSync(temp, { recursive: true });
  const files = [];
  const sourceNames = new Map();
  for (const [source, roles] of sources) {
    let name = path.basename(source);
    if ([...sourceNames.values()].includes(name) && sourceNames.get(source) !== name) name = `${[...roles].join("-")}-${name}`;
    sourceNames.set(source, name);
    copyFileSync(source, path.join(temp, name));
    files.push({ role: [...roles].sort().join("+"), name, sha256: hashFile(source), sizeBytes: statSync(source).size });
  }
  const routes = { patch: [], update: [] };
  for (const artifact of target.installer?.artifacts ?? []) {
    const source = path.resolve(configRoot, artifact.file);
    routes.patch.push({ role: "installer", name: sourceNames.get(source), bucket: "public", key: artifact.key });
  }
  for (const artifact of target.updater?.artifacts ?? []) {
    const source = path.resolve(configRoot, artifact.file);
    const patchKey = artifact.patchKey ?? artifact.key.replace("/updates/", "/installers/");
    routes.patch.push({ role: "updater", name: sourceNames.get(source), signature: sourceNames.get(path.resolve(configRoot, artifact.signature)), bucket: "public", key: patchKey, platform: artifact.platform });
    routes.update.push({ role: "updater", name: sourceNames.get(source), signature: sourceNames.get(path.resolve(configRoot, artifact.signature)), bucket: "private", key: artifact.key, platform: artifact.platform });
  }
  writeJson(path.join(temp, "hardening.json"), { schema: 1, passed: true, checkedAt: new Date().toISOString(), files: files.map((file) => file.sha256) });
  const manifest = {
    schema: 1,
    releaseId,
    app: config.app,
    version: config.version,
    commit,
    platform,
    cacheKey,
    signing: signingIdentity ? { ...signingIdentity, receipts: signingIdentity.receiptInputs.map((file) => ({ file, sha256: hashFile(file) })) } : null,
    files,
    routes,
    inputs: inputHashes,
    tools: toolVersions,
    checkpoints: ["preflight_complete", "build_complete", "signed", "hardened", "sealed"],
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
}

function collectToolVersions(packageManager) {
  const probeDir = mkdtempSync(path.join(tmpdir(), "right-release-tool-probe-"));
  try {
    const rustcVerbose = commandOutputAt("rustc", ["-vV"], probeDir);
    return {
      node: process.version,
      packageManager: `${packageManager} ${commandOutput(packageManager, ["--version"])}`,
      cargo: commandOutputAt("cargo", ["--version"], probeDir),
      rustc: rustcVerbose,
      rustHost: rustcVerbose.match(/^host:\s*(.+)$/m)?.[1] ?? "unknown",
      sccache: commandExists("sccache") ? commandOutput("sccache", ["--version"]) : null,
    };
  } finally {
    rmSync(probeDir, { recursive: true, force: true });
  }
}

async function runProgress(cmd, runArgs, cwd, env, watchDir, diagnosticPath = null) {
  const inactivityMs = Number(process.env.RIGHT_RELEASE_STALL_MS ?? 10 * 60 * 1000);
  const absoluteMs = Number(process.env.RIGHT_RELEASE_ABSOLUTE_MS ?? 90 * 60 * 1000);
  let lastProgress = Date.now();
  const watchDirs = Array.isArray(watchDir) ? watchDir : [watchDir];
  let lastMtime = Math.max(...watchDirs.map(newestMtime));
  const started = Date.now();
  await new Promise((resolve, reject) => {
    child = spawn(cmd, runArgs, { cwd, env, stdio: ["inherit", "pipe", "pipe"], windowsHide: true, shell: process.platform === "win32", detached: process.platform !== "win32" });
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
      if (Date.now() - started > absoluteMs) stop(`absolute limit exceeded (${Math.round(absoluteMs / 60000)}m)`, cpu);
      else if (Date.now() - lastProgress > inactivityMs) stop(`no output, file, or CPU progress for ${Math.round(inactivityMs / 60000)}m`, cpu);
    }, Math.min(30_000, Math.max(250, Math.floor(inactivityMs / 4))));
    const cleanup = () => { clearInterval(monitor); closeWatchers(); };
    const stop = (reason, cpu = null) => {
      cleanup();
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
            `killedAt: ${new Date().toISOString()}`,
            "",
            "--- last output ---",
            tail || "(the step produced no output at all)",
            "",
          ].join("\n"));
          console.error(`right-release: stall diagnostic written to ${diagnosticPath}`);
        } catch { /* diagnostics are best effort; never mask the stall itself */ }
      }
      killTree(child.pid);
      reject(new Error(`release step stalled: ${reason}`));
    };
    child.once("error", (error) => { cleanup(); reject(error); });
    child.once("exit", (code) => { cleanup(); child = null; code === 0 ? resolve() : reject(new Error(`${cmd} exited ${code}`)); });
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

function git(cwd, runArgs) {
  return commandOutputAt("git", runArgs, cwd);
}

function commandOutputAt(cmd, runArgs, cwd) {
  const result = spawnSync(cmd, runArgs, { cwd, encoding: "utf8", windowsHide: true });
  if (result.status !== 0) fail(`${cmd} ${runArgs.join(" ")} failed: ${(result.stderr ?? "").trim()}`);
  return result.stdout.trim();
}

function runChecked(cmd, runArgs, cwd, env = process.env) {
  const result = spawnSync(cmd, runArgs, { cwd, env, stdio: "inherit", windowsHide: true, shell: false });
  if (result.status !== 0) fail(`${cmd} exited ${result.status}`);
}

function killTree(pid) {
  terminateProcessTree(pid);
}

function writeJson(file, value) {
  mkdirSync(path.dirname(file), { recursive: true });
  writeFileSync(file, `${JSON.stringify(value, null, 2)}\n`);
}

function usage(code) {
  console.log("usage: right-release build --platform win|mac [--config right-release.config.mjs] [--dry-run]");
  process.exit(code);
}

function fail(message) {
  console.error(`right-release build: ${message}`);
  throw new Error(message);
}

function throwIfInterrupted() { if (interrupted) throw interrupted; }
