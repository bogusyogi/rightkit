#!/usr/bin/env node
import { spawn, spawnSync } from "node:child_process";
import path from "node:path";
import { fileURLToPath } from "node:url";

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

function spawnCargo(command, args, env) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, { env, stdio: "inherit", windowsHide: true });
    child.once("error", reject);
    child.once("exit", (code, signal) => resolve(code ?? (signal ? 1 : 0)));
  });
}

export async function runCargoGuard(args, { env = process.env, resolveCargo = resolveRealCargo, runHeavy = runHeavyCommand, runLight = spawnCargo } = {}) {
  const cargo = resolveCargo({ env });
  if (shouldGuardCargo(args)) return runHeavy(["--", cargo, ...args], { env });
  return runLight(cargo, args, env);
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  runCargoGuard(process.argv.slice(2)).then((code) => { process.exitCode = code; }).catch((error) => { console.error(`cargo-guard: ${error.message}`); process.exitCode = 1; });
}
