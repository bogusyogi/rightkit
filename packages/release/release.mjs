#!/usr/bin/env node
import { access, readFile, readdir } from "node:fs/promises";
import { existsSync, mkdirSync, readFileSync, realpathSync, unlinkSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { spawn, spawnSync } from "node:child_process";
import { validateRightKitCargoContract } from "./cargo-contract.mjs";
import { assertQaBackdoorContract } from "./qa-contract.mjs";
import { assertLegalReleaseContract } from "./legal-contract.mjs";
import { assertPrimaryReleaseCheckout, dirtyBuildInputs, resolveReleaseBuildInputs } from "./release-invocation.mjs";
import { collectPreflight, formatPreflight, preflightFailures } from "./preflight.mjs";

const TOOL_ROOT = path.dirname(fileURLToPath(import.meta.url));
const HARDENING_SCAN = path.resolve(TOOL_ROOT, "hardeningscan.mjs");
const UPLOAD_LARGE = path.resolve(TOOL_ROOT, "upload-large.mjs");
const SIGN_WINDOWS = path.resolve(TOOL_ROOT, "sign-windows.mjs");
const SIGN_UPDATER = path.resolve(TOOL_ROOT, "sign-updater.mjs");
const RIGHTKIT_VERSIONS = JSON.parse(readFileSync(path.resolve(TOOL_ROOT, "rightkit-versions.json"), "utf8"));
const VERSION = RIGHTKIT_VERSIONS.stagedNpm?.["@rightkit/release"] ?? RIGHTKIT_VERSIONS.npm["@rightkit/release"];
const TIERS = new Set(["patch", "update"]);
const RIGHTKIT_ALLOWED = buildAllowedVersions(
  RIGHTKIT_VERSIONS.npm,
  RIGHTKIT_VERSIONS.stagedNpm,
  RIGHTKIT_VERSIONS.legacyNpm,
);
const RIGHTKIT_CARGO_ALLOWED = buildAllowedVersions(RIGHTKIT_VERSIONS.cargo, RIGHTKIT_VERSIONS.stagedCargo);
const FORBIDDEN_RIGHTKIT_SPEC = /^(?:git|file|link|workspace):|github|github\.com/i;

const args = process.argv.slice(2);
const opts = {
  config: "right-release.config.mjs",
  platform: process.platform === "win32" ? "win" : process.platform === "darwin" ? "mac" : process.platform,
  upload: false,
  install: false,
  dryRun: false,
  skipChecks: false,
  skipHardening: false,
  doctor: false,
  tier: null,
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
  else if (arg === "--skip-hardening") opts.skipHardening = true;
  else if (arg === "--doctor") opts.doctor = true;
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
const sccacheVersion = assertSharedSccachePrerequisite();

const requestedConfigPath = path.resolve(opts.config);
const configPath = opts.doctor ? realpathSync(requestedConfigPath) : requestedConfigPath;
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
await validateRightKitPackageContract(root, config.app);
validateRightKitCargoContract(root, RIGHTKIT_CARGO_ALLOWED, config.app ?? path.basename(root));
const target = config.targets?.[opts.platform];
if (!target) fail(`${config.app ?? "app"} has no ${opts.platform} release target`);
const legalContract = config.legal
  ? assertLegalReleaseContract(root, config.legal, config.app ?? path.basename(root), opts.platform)
  : null;
if (target.signed !== true) {
  fail(`${config.app ?? "app"} ${opts.platform} must declare signed: true; unsigned release targets are forbidden`);
}
if (opts.platform === "win" && !target.sign?.files?.length) {
  fail(`${config.app ?? "app"} win must declare sign.files for the signed release pipeline`);
}
if (opts.upload && target.publishBlocked) fail(`${config.app ?? "app"} ${opts.platform} publish blocked: ${target.publishBlocked}`);
/**
 * Assemble preflight inputs from the app's own files. Kept here (not in
 * preflight.mjs) so the check module stays free of release.mjs's config shape.
 */
function releasePreflight({ config, root, repoRoot, platform }) {
  let version;
  try {
    version = JSON.parse(readFileSync(path.join(root, "package.json"), "utf8")).version;
  } catch {
    version = undefined;
  }
  // Every Cargo.lock the build compiles from; a vendored-OpenSSL edge in any of
  // them means the build needs a real Windows Perl.
  const cargoLockPaths = [
    path.join(root, "src-tauri", "Cargo.lock"),
    path.join(root, "Cargo.lock"),
    ...(config.cargoLocks ?? []).map((entry) => path.resolve(root, entry)),
  ];
  return collectPreflight({
    platform,
    appRoot: root,
    repoRoot,
    app: config.app,
    version,
    cargoLockPaths,
  });
}

if (opts.doctor) {
  const { buildInputs } = resolveReleaseBuildInputs({
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

  // Config above, MACHINE below. Printing config never told anyone whether this
  // box could finish a build; these checks do.
  const checks = releasePreflight({ config, root, repoRoot: doctorInvocation.repoRoot, platform: opts.platform });
  console.log("preflight:");
  console.log(formatPreflight(checks));
  const failures = preflightFailures(checks);
  if (failures.length > 0) {
    fail(`preflight found ${failures.length} blocking problem(s); the build would fail on these`);
  }
  process.exit(0);
}

const releaseLock = acquireReleaseLock(root, config.app ?? path.basename(root), opts.platform, opts.tier);
const mode = "package";
const command = target.package;
if (!command) fail(`${config.app ?? "app"} has no ${opts.platform} ${mode} command`);
if (opts.upload && !target.publish && !target.upload) {
  fail(`${config.app ?? "app"} has no ${opts.platform} upload config`);
}

const started = Date.now();
console.log(`right-release ${VERSION}: ${config.app ?? path.basename(root)} ${opts.platform} ${mode} tier=${opts.tier ?? "neutral"}`);

if (opts.install) await runInstall(config.packageManager, workdir);
if (!opts.skipChecks) {
  for (const script of config.checks ?? []) await runPackageScript(config.packageManager, script, workdir);
}

await runCommand(command, root);

for (const rel of target.artifacts ?? []) {
  await mustExist(path.resolve(root, rel), `missing release artifact: ${rel}`);
}

if (opts.platform === "win" && target.sign?.files?.length) {
  const files = target.sign.files.map((p) => path.resolve(root, p));
  for (const file of files) await mustExist(file, `missing signing artifact: ${file}`);
  await run("node", [SIGN_WINDOWS, ...(opts.dryRun ? ["--dry-run"] : []), ...files], root);
  const updaterFiles = [...new Set((target.updater?.artifacts ?? []).map((artifact) => path.resolve(root, artifact.file)))];
  if (!updaterFiles.length) fail(`${config.app} win must declare updater.artifacts for post-code-signing minisign`);
  await run("node", [SIGN_UPDATER, ...(opts.dryRun ? ["--dry-run"] : []), ...updaterFiles.flatMap((file) => ["--file", file])], root);
}

if (target.postSign) {
  await runCommand(target.postSign, root);
}

if (!opts.skipHardening) {
  const scanTargets = target.hardening ?? target.artifacts ?? [];
  if (scanTargets.length) {
    await run("node", [HARDENING_SCAN, ...scanTargets.map((p) => path.resolve(root, p))], root);
  }
}

if (opts.upload && target.publish) {
  await runCommand(target.publish, root);
}

if (opts.upload && !target.publish) {
  for (const item of target.upload ?? []) {
    const file = path.resolve(root, item.file);
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
  console.error("usage: node release.mjs --tier patch|update [--config right-release.config.mjs] [--platform mac|win] [--upload|--no-upload] [--install] [--dry-run] [--doctor]");
  process.exit(code);
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

// Post-release target/ hygiene: `cargo sweep --installed` deletes artifacts left
// by toolchains rustup no longer has (each update orphans a multi-GB pile per
// app) and never touches anything the current toolchains produced. Runs after
// publish so it cannot race an artifact, and is best-effort: a missing
// cargo-sweep or a sweep failure must never fail a release.
function sweepStaleRustArtifacts() {
  const candidates = [...new Set([root, workdir].flatMap((base) => [base, path.join(base, "src-tauri")]))];
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

async function runCommand(command, root) {
  const cwd = path.resolve(root, command.cwd ?? ".");
  await run(command.cmd, command.args ?? [], cwd, await commandEnv(command, root), command);
}

async function validateRightKitPackageContract(root, appName) {
  const packageJsonPath = path.join(root, "package.json");
  const pkg = JSON.parse(await readFile(packageJsonPath, "utf8"));
  const scripts = pkg.scripts ?? {};
  assertQaBackdoorContract(root, scripts);
  const workflowDir = path.join(root, ".github", "workflows");
  if (existsSync(workflowDir)) {
    const workflows = await readdir(workflowDir);
    if (workflows.length) {
      fail(`${appName ?? pkg.name ?? "app"} hosted workflow files are forbidden: ${workflows.join(", ")}`);
    }
  }
  if (pkg.packageManager !== RIGHTKIT_VERSIONS.packageManager) {
    fail(`${appName ?? pkg.name ?? "app"} packageManager must be ${RIGHTKIT_VERSIONS.packageManager}, got ${pkg.packageManager ?? "<missing>"}`);
  }
  if (JSON.stringify(scripts).includes("../tools/right-release") || JSON.stringify(scripts).includes("../tools/rightkit/packages/release")) {
    fail(`${appName ?? pkg.name ?? "app"} package.json must call the right-release bin, not parent-workspace release source`);
  }
  const rightKitDeps = { ...(pkg.dependencies ?? {}), ...(pkg.devDependencies ?? {}) };
  for (const [name, specifier] of Object.entries(rightKitDeps).filter(([name]) => name.startsWith("@rightkit/"))) {
    if (FORBIDDEN_RIGHTKIT_SPEC.test(String(specifier))) {
      fail(`${appName ?? pkg.name ?? "app"} ${name} must come from the published npm package, not ${specifier}`);
    }
    const allowed = RIGHTKIT_ALLOWED.get(name);
    if (allowed && !allowed.has(specifier)) {
      fail(`${appName ?? pkg.name ?? "app"} ${name} must be one of ${[...allowed].join(", ")}, got ${specifier}`);
    }
  }
  const releaseAllowed = RIGHTKIT_ALLOWED.get("@rightkit/release");
  if (!releaseAllowed?.has(pkg.devDependencies?.["@rightkit/release"])) {
    fail(`${appName ?? pkg.name ?? "app"} must depend on a supported @rightkit/release version in devDependencies`);
  }
}

function buildAllowedVersions(published = {}, staged = {}, legacy = {}) {
  const allowed = new Map();
  for (const [name, value] of [
    ...Object.entries(published),
    ...Object.entries(staged),
    ...Object.entries(legacy),
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

function run(cmd, runArgs, cwd, env = {}, options = {}) {
  const printable = `${cmd} ${runArgs.join(" ")}`.trim();
  const releaseEnv = opts.tier ? { RIGHT_RELEASE_TIER: opts.tier } : {};
  if (opts.dryRun) {
    const timeout = options.timeoutMs ? ` timeout=${options.timeoutMs}ms` : "";
    const tier = opts.tier ? `RIGHT_RELEASE_TIER=${opts.tier} ` : "";
    console.log(`dry-run: (${cwd}) ${tier}${printable}${timeout}`);
    return Promise.resolve();
  }
  const started = Date.now();
  return new Promise((resolve) => {
    const child = spawn(cmd, runArgs, {
      cwd,
      env: { ...process.env, ...releaseEnv, ...env },
      stdio: "inherit",
      shell: process.platform === "win32",
      windowsHide: true,
    });
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
      resolve();
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
  if (!pid) return;
  if (process.platform === "win32") {
    spawnSync("taskkill", ["/PID", String(pid), "/T", "/F"], { stdio: "ignore" });
    return;
  }
  const children = spawnSync("pgrep", ["-P", String(pid)], { encoding: "utf8" });
  for (const child of (children.stdout ?? "").split(/\s+/).filter(Boolean)) {
    killProcessTree(Number(child));
  }
  try {
    process.kill(pid, "SIGKILL");
  } catch {
    // already gone
  }
}
