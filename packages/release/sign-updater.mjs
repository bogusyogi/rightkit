#!/usr/bin/env node
import { existsSync, rmSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { signMacUpdaterFiles } from "./macos-signing-session.mjs";
import { refuseManagedEnvSecrets } from "./managed-host-secrets.mjs";
import { execWithUpdaterCredential } from "./protected-credentials.mjs";
import { resolvePnpmCli } from "./development.mjs";

export const UPDATER_SIGNING_SECRET = "updater-signing-key";

export function runSignUpdater(args, {
  env = process.env, platform = process.platform, cwd = process.cwd(),
  execCredential = execWithUpdaterCredential, resolvePnpm = resolvePnpmCli,
  signMac = signMacUpdaterFiles, exists = existsSync, remove = rmSync, log = console.log,
} = {}) {
  const files = [];
  let dryRun = false;
  for (let i = 0; i < args.length; i++) {
    if (args[i] === "--file") {
      if (!args[i + 1] || args[i + 1].startsWith("--")) throw new Error("--file requires an artifact path");
      files.push(path.resolve(cwd, args[++i]));
    } else if (args[i] === "--key-file") throw new Error("--key-file is removed: updater keys come from the OS credential provider");
    else if (args[i] === "--dry-run") dryRun = true;
    else throw new Error(`unknown argument: ${args[i]}`);
  }
  if (!files.length) throw new Error("at least one --file is required");
  if (!dryRun) {
    refuseManagedEnvSecrets(env, ["TAURI_SIGNING_PRIVATE_KEY", "TAURI_SIGNING_PRIVATE_KEY_PASSWORD"]);
    for (const key of Object.keys(env)) {
      if (["TAURI_SIGNING_PRIVATE_KEY", "TAURI_SIGNING_PRIVATE_KEY_PASSWORD"].includes(key.toUpperCase()) && env[key]) {
        throw new Error("updater signing key/password must come from the OS credential provider");
      }
    }
    for (const file of files) if (!exists(file)) throw new Error(`updater artifact not found: ${file}`);
  }
  const signingEnv = { ...env };
  for (const key of Object.keys(signingEnv)) {
    if (["TAURI_SIGNING_PRIVATE_KEY", "TAURI_SIGNING_PRIVATE_KEY_PASSWORD"].includes(key.toUpperCase())) delete signingEnv[key];
  }
  for (const file of files) {
    log(`${dryRun ? "[dry-run] " : ""}pnpm exec tauri signer sign ${file}`);
    if (dryRun) continue;
    remove(`${file}.sig`, { force: true });
    if (platform === "darwin") continue;
    const result = execCredential(process.execPath,
      [resolvePnpm({ root: cwd, env: signingEnv }), "exec", "tauri", "signer", "sign", file],
      { cwd, env: signingEnv });
    if (result.error || result.status !== 0) {
      const error = new Error(`updater credential provider/signing failed (exit ${result.status ?? "unavailable"})`);
      error.exitCode = result.status ?? 1;
      throw error;
    }
  }
  if (!dryRun && platform === "darwin") signMac(files, { env: signingEnv, cwd });
  return { files, dryRun };
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try { runSignUpdater(process.argv.slice(2)); }
  catch (error) {
    console.error(`sign-updater: ${error.code ? `[${error.code}] ` : ""}${error.message}`);
    process.exitCode = error.exitCode ?? 1;
  }
}
