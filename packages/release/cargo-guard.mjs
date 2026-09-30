#!/usr/bin/env node
import { spawn, spawnSync } from "./background-process.mjs";
import { createHash } from "node:crypto";
import { existsSync, readFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { resolveSharedCacheRoot, resolveStableRoot } from "./cache-policy.mjs";
import { brokerManagedHost } from "./cargo-contract.mjs";
import { runHeavyCommand } from "./heavy-command.mjs";

const LIGHT_COMMANDS = new Set(["fmt", "metadata", "tree", "fetch", "search", "locate-project", "read-manifest", "help", "version", "--version", "-V"]);
const GLOBAL_OPTIONS_WITH_VALUE = new Set(["--color", "--config", "-Z"]);

export function cargoSubcommand(args) {
  for (let index = 0; index < args.length; index += 1) {
    const value = args[index];
    if (value.startsWith("+") || value.startsWith("--color=") || value.startsWith("--config=")) continue;
    if (GLOBAL_OPTIONS_WITH_VALUE.has(value)) { index += 1; continue; }
    if (value.startsWith("-Z")) continue;
    if (!value.startsWith("-") || value === "--version" || value === "-V") return value;
  }
  return null;
}

export function shouldGuardCargo(args) {
  const command = cargoSubcommand(args);
  return command !== null && !LIGHT_COMMANDS.has(command);
}

export function computePolicyPath({ env = process.env, platform = process.platform, home = os.userInfo().homedir } = {}) {
  if (env.RIGHTSUITE_COMPUTE_POLICY) return path.resolve(env.RIGHTSUITE_COMPUTE_POLICY);
  if (platform === "win32" || platform === "win") {
    return path.win32.join(env.LOCALAPPDATA || path.win32.join(home, "AppData", "Local"), "RightSuite", "compute-policy.json");
  }
  if (platform === "darwin" || platform === "mac") return path.join(home, "Library", "Application Support", "RightSuite", "compute-policy.json");
  const base = resolveStableRoot({
    platform,
    env,
    home,
    otherBaseSegments: [".config"],
    otherSegments: ["rightsuite"],
  });
  return path.join(base, "compute-policy.json");
}

export function loadComputePolicy(options = {}) {
  const policyPath = computePolicyPath(options);
  if (!(options.exists ?? existsSync)(policyPath)) return { schemaVersion: 1, cargoTest: "allow" };
  const policy = JSON.parse((options.read ?? readFileSync)(policyPath, "utf8"));
  if (!policy || Object.keys(policy).sort().join(",") !== "cargoTest,schemaVersion"
      || policy.schemaVersion !== 1 || !["allow", "deny"].includes(policy.cargoTest)) {
    throw new Error(`invalid compute policy: ${policyPath}`);
  }
  return policy;
}

function shellSegments(command) {
  const segments = [[]];
  let word = "", quote = null, escaped = false;
  const push = () => { if (word) segments.at(-1).push(word); word = ""; };
  for (const char of String(command)) {
    if (escaped) { word += char; escaped = false; continue; }
    if (char === "\\" && quote !== "'") { escaped = true; continue; }
    if (quote) { if (char === quote) quote = null; else word += char; continue; }
    if (char === "'" || char === '"') { quote = char; continue; }
    if (/\s/.test(char)) { push(); if (char === "\n" && segments.at(-1).length) segments.push([]); continue; }
    if (";&|".includes(char)) { push(); if (segments.at(-1).length) segments.push([]); continue; }
    word += char;
  }
  push();
  return segments.filter((segment) => segment.length);
}

function segmentRunsCargoTest(segment) {
  let index = 0;
  while (/^[A-Za-z_][A-Za-z0-9_]*=/.test(segment[index] ?? "")) index += 1;
  if (path.basename(segment[index] ?? "").toLowerCase() === "env") {
    index += 1;
    while ((segment[index] ?? "").startsWith("-") || /^[A-Za-z_][A-Za-z0-9_]*=/.test(segment[index] ?? "")) index += 1;
  }
  const executable = path.basename(segment[index] ?? "").toLowerCase();
  if (["cargo", "cargo.exe"].includes(executable)) return cargoSubcommand(segment.slice(index + 1)) === "test";
  if (["rustup", "rustup.exe"].includes(executable) && segment[index + 1] === "run") {
    const cargoIndex = segment.findIndex((value, offset) => offset > index + 1 && ["cargo", "cargo.exe"].includes(path.basename(value).toLowerCase()));
    return cargoIndex >= 0 && cargoSubcommand(segment.slice(cargoIndex + 1)) === "test";
  }
  if (["sh", "bash", "zsh"].includes(executable)) {
    const commandIndex = segment.findIndex((value, offset) => offset > index && value === "-c");
    return commandIndex >= 0 && shellRunsCargoTest(segment[commandIndex + 1] ?? "");
  }
  return false;
}

export function shellRunsCargoTest(command) {
  return shellSegments(command).some(segmentRunsCargoTest);
}

export function assertComputePolicyAllowsCargo(args, options = {}) {
  if (cargoSubcommand(args) === "test" && loadComputePolicy(options).cargoTest === "deny") {
    throw new Error("cargo test denied by global RightSuite compute policy");
  }
}

export function checkHookInput(input, options = {}) {
  const event = JSON.parse(String(input));
  const command = event?.tool_input?.command ?? event?.toolInput?.command ?? "";
  if (loadComputePolicy(options).cargoTest === "deny" && shellRunsCargoTest(command)) {
    throw new Error("cargo test denied by global RightSuite compute policy");
  }
}

export function resolveRealCargo({ env = process.env, run = spawnSync } = {}) {
  if (env.RIGHTSUITE_REAL_CARGO) return path.resolve(env.RIGHTSUITE_REAL_CARGO);
  const rustup = process.platform === "win32" ? "rustup.exe" : "rustup";
  const result = run(rustup, ["which", "cargo"], { encoding: "utf8", windowsHide: true });
  const cargo = String(result.stdout ?? "").trim();
  if (result.status !== 0 || !cargo) throw new Error(`cargo-guard could not resolve real Cargo: ${String(result.stderr ?? "").trim()}`);
  return path.resolve(cargo);
}

export function resolveRealRustc({ env = process.env, run = spawnSync } = {}) {
  if (env.RIGHTSUITE_REAL_RUSTC) return path.resolve(env.RIGHTSUITE_REAL_RUSTC);
  const rustup = process.platform === "win32" ? "rustup.exe" : "rustup";
  const result = run(rustup, ["which", "rustc"], { encoding: "utf8", windowsHide: true });
  const rustc = String(result.stdout ?? "").trim();
  if (result.status !== 0 || !rustc) throw new Error(`cargo-guard could not resolve real rustc: ${String(result.stderr ?? "").trim()}`);
  return path.resolve(rustc);
}

// Locate the broker's own managed cargo shim on PATH (the agent-bin launcher
// that routes into the build broker). Used to hand a whole invocation to the
// broker on a managed host instead of resolving & running the real toolchain
// ourselves — which would bypass managed build control entirely.
export function resolveManagedCargoShim({ env = process.env, platform = process.platform, exists = existsSync } = {}) {
  const win = platform === "win32" || platform === "win";
  const delimiter = win ? ";" : ":";
  const agentBin = /(?:^|[\\/])(?:\.rightkit-managed|rightkitmanagedagent)[\\/]agent-bin[\\/]?$/i;
  for (const directory of String(env.PATH ?? "").split(delimiter).filter(Boolean)) {
    if (!agentBin.test(directory)) continue;
    for (const name of win ? ["cargo.exe", "cargo.cmd"] : ["cargo"]) {
      const candidate = path.join(directory, name);
      if (exists(candidate)) return candidate;
    }
  }
  return null;
}

function pathApi(platform) {
  return platform === "win32" || platform === "win" ? path.win32 : path.posix;
}

export function cargoProjectRoot(args, { cwd = process.cwd(), platform = process.platform, exists = existsSync } = {}) {
  const paths = pathApi(platform);
  const manifestIndex = args.findIndex((value) => value === "--manifest-path");
  const manifestValue = args.find((value) => value.startsWith("--manifest-path="))?.slice("--manifest-path=".length)
    ?? (manifestIndex >= 0 ? args[manifestIndex + 1] : null);
  let current = paths.resolve(manifestValue ? paths.dirname(paths.resolve(cwd, manifestValue)) : cwd);
  let selected = null;
  for (;;) {
    if (exists(paths.join(current, "Cargo.toml"))) selected = current;
    const parent = paths.dirname(current);
    if (parent === current) break;
    current = parent;
  }
  return selected ?? paths.resolve(cwd);
}

function assertInsideCache(candidate, cacheRoot, label, { cwd, platform }) {
  const paths = pathApi(platform);
  const resolved = paths.resolve(cwd, candidate);
  const relative = paths.relative(paths.resolve(cacheRoot), resolved);
  if (relative === ".." || relative.startsWith(`..${paths.sep}`) || paths.isAbsolute(relative)) {
    throw new Error(`${label} must stay inside shared cache root ${cacheRoot}; got ${candidate}`);
  }
  return resolved;
}

export function cargoCacheEnvironment(args, {
  env = process.env, cwd = process.cwd(), platform = process.platform, exists = existsSync, isBrokerHost = brokerManagedHost,
} = {}) {
  const paths = pathApi(platform);
  const cacheRoot = resolveSharedCacheRoot({ platform, env });
  const projectRoot = cargoProjectRoot(args, { cwd, platform, exists });
  const projectName = paths.basename(projectRoot).replace(/[^A-Za-z0-9._-]+/g, "-") || "cargo";
  const projectHash = createHash("sha256").update(platform === "win32" || platform === "win" ? projectRoot.toLowerCase() : projectRoot).digest("hex").slice(0, 12);
  const defaultTarget = paths.join(cacheRoot, "dev-targets", `${projectName}-${projectHash}`);
  const defaultSccache = paths.join(cacheRoot, "sccache");
  const targetOption = args.find((value) => value.startsWith("--target-dir="));
  const targetIndex = args.findIndex((value) => value === "--target-dir");
  const explicitTarget = targetOption?.slice("--target-dir=".length) ?? (targetIndex >= 0 ? args[targetIndex + 1] : null);
  // On a broker-managed host, CARGO_TARGET_DIR is not this cache's to police: the
  // build broker unconditionally overrides it to its own per-namespace workspace
  // outside the shared cache root, so asserting it stays inside cacheRoot is a
  // spurious failure, not a real violation. Pass the broker's own value through
  // untouched instead of rejecting it.
  const broker = isBrokerHost(env, platform, exists);
  if (explicitTarget && !broker) assertInsideCache(explicitTarget, cacheRoot, "--target-dir", { cwd, platform });
  const targetDir = broker
    ? (env.CARGO_TARGET_DIR ?? explicitTarget ?? defaultTarget)
    : env.CARGO_TARGET_DIR
      ? assertInsideCache(env.CARGO_TARGET_DIR, cacheRoot, "CARGO_TARGET_DIR", { cwd, platform })
      : defaultTarget;
  const sccacheDir = env.SCCACHE_DIR
    ? assertInsideCache(env.SCCACHE_DIR, cacheRoot, "SCCACHE_DIR", { cwd, platform })
    : defaultSccache;
  const wrapper = String(env.RUSTC_WRAPPER ?? "sccache");
  if (!["sccache", "sccache.exe"].includes(paths.basename(wrapper).toLowerCase())) {
    throw new Error(`RUSTC_WRAPPER must be sccache; got ${wrapper || "<empty>"}`);
  }
  return {
    ...env,
    CARGO_TARGET_DIR: targetDir,
    RUSTC_WRAPPER: env.RUSTC_WRAPPER || "sccache",
    SCCACHE_DIR: sccacheDir,
    SCCACHE_CACHE_SIZE: env.SCCACHE_CACHE_SIZE || "32G",
    RIGHTSUITE_CARGO_CACHE_GUARD: "1",
  };
}

function spawnCargo(command, args, env) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, { env, stdio: "inherit", windowsHide: true });
    child.once("error", reject);
    child.once("exit", (code, signal) => resolve(code ?? (signal ? 1 : 0)));
  });
}

export async function runCargoGuard(args, { env = process.env, policyOptions = {}, resolveCargo = resolveRealCargo, resolveRustc = resolveRealRustc, runHeavy = runHeavyCommand, runLight = spawnCargo, resolveShim = resolveManagedCargoShim, isBrokerHost = brokerManagedHost } = {}) {
  assertComputePolicyAllowsCargo(args, { ...policyOptions, env });
  // On a broker-managed host the build broker owns the toolchain, cache, and
  // quotas. Resolving the real Cargo here and running it via runHeavy would
  // bypass the broker (and, on Windows, resolveReal* itself fails: `rustup
  // which` routes through the managed shim into the broker's isolated
  // RUSTUP_HOME, which has no default toolchain). Hand the whole invocation to
  // the broker's own cargo shim instead — everything through the broker.
  if (isBrokerHost(env)) {
    const shim = resolveShim({ env });
    if (shim) return runLight(shim, args, env);
    // No shim found despite a broker signal: fall through to legacy resolution
    // rather than silently dropping the guard.
  }
  const cargo = resolveCargo({ env });
  if (shouldGuardCargo(args)) {
    const guarded = cargoCacheEnvironment(args, { env, isBrokerHost });
    // Pin RUSTC on every platform, not just Windows: cargo otherwise resolves
    // bare `rustc` from its child PATH, and on this Mac that reached a stale
    // 1.78.0 shim while cargo 1.97 passed --check-cfg — failing any crate
    // build with "the `-Z unstable-options` flag must also be passed".
    if (!guarded.RUSTC) guarded.RUSTC = resolveRustc({ env });
    return runHeavy(["--", cargo, ...args], { env: guarded });
  }
  return runLight(cargo, args, env);
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const args = process.argv.slice(2);
  if (args[0] === "--check-hook-base64") {
    try { checkHookInput(Buffer.from(args[1] ?? "", "base64url").toString("utf8")); }
    catch (error) { console.error(`cargo-guard: ${error.message}`); process.exitCode = 2; }
  } else {
    runCargoGuard(args).then((code) => { process.exitCode = code; }).catch((error) => { console.error(`cargo-guard: ${error.message}`); process.exitCode = 1; });
  }
}
