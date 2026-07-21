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
import { spawn, spawnSync } from "node:child_process";
import { fileURLToPath, pathToFileURL } from "node:url";
import { assertPrimaryReleaseCheckout, dirtyBuildInputs } from "./release-invocation.mjs";
import { commandOutputPortable, releaseEnvironment, resolveReleaseLayout, runBuildStateMachine, verifySealedRelease, watchProgress } from "./release-state.mjs";
import { acquireCacheLease, acquireSuiteBuildSlot, applyCachePrune, assertWriteVolumeFloors, inspectCache, inspectWriteVolumes, markCacheEntrySuccessful, planCachePrune, readCachePolicy, resolveCacheLayout, resolveSharedCacheIdentity, resolveSharedCacheRoot } from "./cache-policy.mjs";

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

const invocationRoot = path.dirname(path.resolve(configName));
const repoRoot = git(invocationRoot, ["rev-parse", "--show-toplevel"]);
assertPrimaryReleaseCheckout(repoRoot);
const relativeConfig = path.relative(repoRoot, path.resolve(configName)).replaceAll("\\", "/");
if (relativeConfig.startsWith("../")) fail("release config must live inside the Git repository");
const vaultRoot = resolveReleaseLayout({ repoRoot, configPath: path.resolve(configName) }).vaultRoot;
mkdirSync(path.join(vaultRoot, "locks"), { recursive: true });
const lock = acquireLock(path.join(vaultRoot, "locks", `${platform}.lock.json`));
let child = null;
let suiteSlot;
let cacheLease;
let interrupted;

for (const signal of ["SIGINT", "SIGTERM", "SIGHUP"]) {
  process.once(signal, () => {
    if (child?.pid) killTree(child.pid);
    interrupted = new Error(`release interrupted by ${signal}`);
    process.exitCode = signal === "SIGINT" ? 130 : 143;
  });
}

try {
  throwIfInterrupted();
  if (!dryRun) git(repoRoot, ["fetch", "origin", "main"]);
  const commit = git(repoRoot, ["rev-parse", dryRun ? "HEAD" : "origin/main"]);
  const shortCommit = commit.slice(0, 8);
  const worktree = path.join(vaultRoot, "worktrees", `${platform}-${shortCommit}`);
  if (!existsSync(worktree)) {
    if (dryRun) fail(`dry-run needs an existing worktree: ${worktree}`);
    mkdirSync(path.dirname(worktree), { recursive: true });
    runChecked("git", ["worktree", "add", "--detach", worktree, commit], repoRoot);
  } else if (git(worktree, ["rev-parse", "HEAD"]) !== commit) {
    fail(`release worktree exists at the wrong commit: ${worktree}`);
  }

  const worktreeConfigPath = path.join(worktree, relativeConfig);
  const appRoot = path.dirname(worktreeConfigPath);
  const layout = resolveReleaseLayout({ repoRoot, configPath: path.resolve(configName) });
  const config = (await import(`${pathToFileURL(worktreeConfigPath).href}?commit=${commit}`)).default;
  if (!config?.app || !config?.version) fail("release config must expose app and version");
  if (!/^[A-Za-z0-9][A-Za-z0-9._-]*$/.test(config.app) || !/^[A-Za-z0-9][A-Za-z0-9._+-]*$/.test(config.version)) {
    fail("app and version must be safe release-id components");
  }
  const target = config.targets?.[platform];
  if (!target?.package) fail(`${config.app} has no ${platform} package command`);
  const dirtyInputs = dirtyBuildInputs(repoRoot, repoBuildInputs(repoRoot, invocationRoot, relativeConfig, target.buildInputs ?? config.buildInputs), commit);
  if (dirtyInputs.length > 0) fail(`dirty files can change the packaged app; resolve them before release:\n${dirtyInputs.map((file) => `- ${file}`).join("\n")}`);
  const requiredInputs = inputPaths(appRoot, worktreeConfigPath, target);
  const toolVersions = collectToolVersions(config.packageManager ?? "pnpm");
  const inputHashes = hashInputs(requiredInputs);
  const cargoLock = requiredInputs.find((file) => /Cargo\.lock$/i.test(file));
  const cargoToml = requiredInputs.find((file) => /Cargo\.toml$/i.test(file));
  const cacheIdentity = resolveSharedCacheIdentity({ cargoLockPath: cargoLock, cargoTomlPath: cargoToml, rustcVerbose: toolVersions.rustc, targetTriple: target.cargoTarget ?? target.targetTriple ?? target.rustTarget ?? toolVersions.rustHost, profile: target.profile ?? "release" });
  const cacheKey = cacheIdentity.fingerprint;
  const cacheMode = process.env.RIGHT_RELEASE_CACHE_MODE ?? (platform === "mac" ? "shared" : "legacy");
  if (cacheMode !== "legacy" && cacheMode !== "shared") fail("RIGHT_RELEASE_CACHE_MODE must be legacy or shared");
  const sharedCacheRoot = cacheMode === "shared" ? resolveSharedCacheRoot({ platform, env: process.env }) : undefined;
  const sharedLayout = cacheMode === "shared"
    ? resolveCacheLayout({ cacheRoot: sharedCacheRoot, platform, architecture: cacheIdentity.architecture, app: config.app, fingerprint: cacheKey, kind: "release" })
    : undefined;
  if (cacheMode === "shared") assertSccacheVersion(toolVersions.sccache);
  const env = {
    ...process.env,
    ...releaseEnvironment({ root: vaultRoot, cacheRoot: sharedCacheRoot, platform, architecture: cacheIdentity.architecture, app: config.app, cacheKey, kind: "release", appRoot, mode: cacheMode }),
    RIGHT_RELEASE_REPO_ROOT: layout.repoRoot,
    RIGHT_RELEASE_APP_ROOT: layout.appRoot,
  };
  if (cacheMode === "legacy") delete env.RIGHT_RELEASE_CACHE_OWNER;
  if (cacheMode === "shared" && env.RIGHT_RELEASE_CACHE_OWNER !== "rightkit-v2") fail("shared cache ownership token was not configured");
  if (cacheMode === "legacy" && !commandExists("sccache")) delete env.RUSTC_WRAPPER;
  if (cacheMode === "shared") {
    suiteSlot = acquireSuiteBuildSlot({ layout: sharedLayout, pid: process.pid, argv: process.argv, waitMs: Number(process.env.BUILD_GUARD_WAIT_SECS ?? 900) * 1000 });
    cacheLease = acquireCacheLease({ layout: sharedLayout, pid: process.pid, argv: process.argv });
  }
  throwIfInterrupted();
  const releaseId = `${config.app}-${config.version}-${shortCommit}`;
  const platformDir = platform === "win" ? "windows" : "mac";
  const buildRoot = path.join(vaultRoot, "build", config.app, config.version, shortCommit, platformDir);
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
    tools: toolVersions,
    createdAt: new Date().toISOString(),
  };

  const result = await runBuildStateMachine({
    root: repoRoot,
    app: config.app,
    version: config.version,
    commit,
    platform,
    requiredInputs,
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
        mkdirSync(buildRoot, { recursive: true });
        mkdirSync(stateRoot, { recursive: true });
        writeJson(path.join(buildRoot, "release-inputs.lock.json"), lockPayload);
        writeJson(path.join(stateRoot, "release-inputs.lock.json"), lockPayload);
        checkpoint(stateRoot, "preflight_complete");
      },
      prepare: async () => {
        throwIfInterrupted();
        const cacheTarget = env.CARGO_TARGET_DIR;
        const targetLink = path.join(appRoot, "src-tauri", "target");
        mkdirSync(cacheTarget, { recursive: true });
        if (!existsSync(targetLink)) symlinkSync(cacheTarget, targetLink, process.platform === "win32" ? "junction" : "dir");
        else if (realpathSync(targetLink) !== realpathSync(cacheTarget)) {
          fail(`worktree target exists and is not the release cache link: ${targetLink}`);
        }
        await runProgress(config.packageManager ?? "pnpm", ["install", "--frozen-lockfile"], appRoot, env, cacheTarget);
      },
      build: async () => {
        throwIfInterrupted();
        await runProgress(
          process.execPath,
          [WORKER, "--config", worktreeConfigPath, "--platform", platform, "--no-upload"],
          appRoot,
          env,
          [env.CARGO_TARGET_DIR, path.join(appRoot, ".cache")],
        );
        checkpoint(stateRoot, "build_complete");
        checkpoint(stateRoot, "signed");
        checkpoint(stateRoot, "hardened");
      },
      seal: async ({ sealedDir }) => {
        sealRelease({ configRoot: path.dirname(worktreeConfigPath), sealedDir, releaseId, config, target, platform, commit, inputHashes, toolVersions, cacheKey });
        verifySealedRelease(sealedDir);
        if (cacheMode === "shared") markCacheEntrySuccessful({ layout: sharedLayout });
        checkpoint(stateRoot, "sealed");
      },
    },
  });
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
  lock.release();
}

function inputPaths(appRoot, configPath, target) {
  const candidates = [
    configPath,
    path.join(appRoot, "package.json"),
    path.join(appRoot, "pnpm-lock.yaml"),
    path.join(appRoot, "src-tauri", "Cargo.toml"),
    path.join(appRoot, "src-tauri", "Cargo.lock"),
    ...(target.preflight?.files ?? []).map((file) => path.resolve(appRoot, expandEnv(file))),
  ];
  const required = [...new Set(candidates)];
  for (const file of required) if (!existsSync(file)) fail(`missing required release input: ${file}`);
  return required;
}

function repoBuildInputs(repoRoot, appRoot, relativeConfig, configured) {
  if (!Array.isArray(configured?.include) || configured.include.length === 0) {
    fail("release config must declare non-empty buildInputs.include paths");
  }
  const appPrefix = path.relative(repoRoot, appRoot).replaceAll("\\", "/");
  const qualify = (pattern) => [appPrefix, pattern.replaceAll("\\", "/").replace(/^\.\//, "")].filter(Boolean).join("/");
  return {
    include: [
      relativeConfig,
      qualify("package.json"),
      qualify("pnpm-lock.yaml"),
      ...configured.include.map(qualify),
    ],
    exclude: (configured.exclude ?? []).map(qualify),
    json: Object.fromEntries(Object.entries(configured.json ?? {}).map(([file, fields]) => [qualify(file), fields])),
  };
}

function hashInputs(files) {
  return Object.fromEntries(files.map((file) => [path.relative(path.dirname(path.dirname(file)), file), hashFile(file)]));
}

function hashFile(file) {
  return createHash("sha256").update(readFileSync(file)).digest("hex");
}

function sqlCipherFeatures(text) {
  return [...text.matchAll(/features\s*=\s*\[([^\]]+)\]/g)]
    .flatMap((match) => match[1].match(/"([^"]+)"/g) ?? [])
    .map((value) => value.slice(1, -1))
    .filter((value) => /sqlcipher|openssl/i.test(value));
}

function sealRelease({ configRoot, sealedDir, releaseId, config, target, platform, commit, inputHashes, toolVersions, cacheKey }) {
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
  const rustcVerbose = commandOutput("rustc", ["-vV"]);
  return {
    node: process.version,
    packageManager: `${packageManager} ${commandOutput(packageManager, ["--version"])}`,
    cargo: commandOutput("cargo", ["--version"]),
    rustc: rustcVerbose,
    rustHost: rustcVerbose.match(/^host:\s*(.+)$/m)?.[1] ?? "unknown",
    sccache: commandExists("sccache") ? commandOutput("sccache", ["--version"]) : null,
  };
}

async function runProgress(cmd, runArgs, cwd, env, watchDir) {
  const inactivityMs = Number(process.env.RIGHT_RELEASE_STALL_MS ?? 10 * 60 * 1000);
  const absoluteMs = Number(process.env.RIGHT_RELEASE_ABSOLUTE_MS ?? 90 * 60 * 1000);
  let lastProgress = Date.now();
  const watchDirs = Array.isArray(watchDir) ? watchDir : [watchDir];
  let lastMtime = Math.max(...watchDirs.map(newestMtime));
  const started = Date.now();
  await new Promise((resolve, reject) => {
    child = spawn(cmd, runArgs, { cwd, env, stdio: ["inherit", "pipe", "pipe"], windowsHide: true, shell: process.platform === "win32" });
    const closeWatchers = watchProgress(watchDirs, () => { lastProgress = Date.now(); });
    for (const [stream, output] of [[child.stdout, process.stdout], [child.stderr, process.stderr]]) {
      stream.on("data", (chunk) => { lastProgress = Date.now(); output.write(chunk); });
    }
    const monitor = setInterval(() => {
      const mtime = Math.max(...watchDirs.map(newestMtime));
      if (mtime > lastMtime) { lastMtime = mtime; lastProgress = Date.now(); }
      if (Date.now() - started > absoluteMs) stop(`absolute limit exceeded (${Math.round(absoluteMs / 60000)}m)`);
      else if (Date.now() - lastProgress > inactivityMs) stop(`no output or file progress for ${Math.round(inactivityMs / 60000)}m`);
    }, Math.min(30_000, Math.max(250, Math.floor(inactivityMs / 4))));
    const cleanup = () => { clearInterval(monitor); closeWatchers(); };
    const stop = (reason) => { cleanup(); killTree(child.pid); reject(new Error(`release step stalled: ${reason}`)); };
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
  if (!pid) return;
  if (process.platform === "win32") spawnSync("taskkill", ["/PID", String(pid), "/T", "/F"], { stdio: "ignore", windowsHide: true });
  else { try { process.kill(-pid, "SIGTERM"); } catch { try { process.kill(pid, "SIGTERM"); } catch { /* gone */ } } }
}

function expandEnv(value) {
  return String(value).replace(/%([^%]+)%/g, (_, name) => process.env[name] ?? "").replace(/\$\{([^}]+)\}/g, (_, name) => process.env[name] ?? "");
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
