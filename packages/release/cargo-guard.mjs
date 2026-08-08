#!/usr/bin/env node
import { spawn, spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { resolveSharedCacheRoot } from "./cache-policy.mjs";
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
  env = process.env, cwd = process.cwd(), platform = process.platform, exists = existsSync,
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
  if (explicitTarget) assertInsideCache(explicitTarget, cacheRoot, "--target-dir", { cwd, platform });
  const targetDir = env.CARGO_TARGET_DIR
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

export async function runCargoGuard(args, { env = process.env, resolveCargo = resolveRealCargo, resolveRustc = resolveRealRustc, runHeavy = runHeavyCommand, runLight = spawnCargo } = {}) {
  const cargo = resolveCargo({ env });
  if (shouldGuardCargo(args)) {
    const guarded = cargoCacheEnvironment(args, { env });
    if (process.platform === "win32" && !guarded.RUSTC) guarded.RUSTC = resolveRustc({ env });
    return runHeavy(["--", cargo, ...args], { env: guarded });
  }
  return runLight(cargo, args, env);
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  runCargoGuard(process.argv.slice(2)).then((code) => { process.exitCode = code; }).catch((error) => { console.error(`cargo-guard: ${error.message}`); process.exitCode = 1; });
}
