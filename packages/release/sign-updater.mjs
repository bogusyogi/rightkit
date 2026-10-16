#!/usr/bin/env node
import { spawnSync } from "./background-process.mjs";
import { existsSync, readFileSync, rmSync } from "node:fs";
import path from "node:path";
import { signMacUpdaterFiles } from "./macos-signing-session.mjs";
import { refuseManagedEnvSecrets, MANAGED_HOST_ENV_SECRET_REFUSED } from "./managed-host-secrets.mjs";

const args = process.argv.slice(2);
const files = [];
let dryRun = false;
let keyFile = process.env.RIGHT_UPDATER_KEY_FILE || "";
for (let i = 0; i < args.length; i++) {
  if (args[i] === "--file") files.push(path.resolve(args[++i]));
  else if (args[i] === "--key-file") keyFile = args[++i] ?? "";
  else if (args[i] === "--dry-run") dryRun = true;
  else fail(`unknown argument: ${args[i]}`);
}
if (!files.length) fail("at least one --file is required");

const env = { ...process.env };
if (!dryRun) {
  try { refuseManagedEnvSecrets(process.env, ["TAURI_SIGNING_PRIVATE_KEY", "TAURI_SIGNING_PRIVATE_KEY_PASSWORD"]); }
  catch (error) { fail(`[${MANAGED_HOST_ENV_SECRET_REFUSED}] ${error.message}`); }
}
if (!dryRun && !env.TAURI_SIGNING_PRIVATE_KEY) {
  if (process.platform === "win32") {
    if (!keyFile) fail("updater key path required on win32: pass --key-file or set RIGHT_UPDATER_KEY_FILE (release config: targets.win.updater.keyFile), or set TAURI_SIGNING_PRIVATE_KEY");
    const key = path.resolve(keyFile.replace(/%([^%]+)%/g, (m, name) => process.env[name] ?? m));
    if (!existsSync(key)) fail(`updater key not found: ${key}`);
    env.TAURI_SIGNING_PRIVATE_KEY = readFileSync(key, "utf8");
  } else if (process.platform !== "darwin") {
    fail("TAURI_SIGNING_PRIVATE_KEY is required on this platform");
  }
}
env.TAURI_SIGNING_PRIVATE_KEY_PASSWORD ??= "";

for (const file of files) {
  if (!dryRun && !existsSync(file)) fail(`updater artifact not found: ${file}`);
  rmSync(`${file}.sig`, { force: true });
  console.log(`${dryRun ? "[dry-run] " : ""}pnpm exec tauri signer sign ${file}`);
  if (dryRun) continue;
  if (process.platform === "darwin") continue;
  const result = process.platform === "win32"
    ? spawnSync(
        process.env.ComSpec || "cmd.exe",
        ["/d", "/s", "/c", `pnpm exec tauri signer sign ${file}`],
        {
          cwd: process.cwd(),
          env,
          stdio: "inherit",
        },
      )
    : spawnSync("pnpm", ["exec", "tauri", "signer", "sign", file], {
    cwd: process.cwd(),
    env,
    stdio: "inherit",
  });
  if (result.status !== 0) process.exit(result.status ?? 1);
}
if (!dryRun && process.platform === "darwin") {
  try { signMacUpdaterFiles(files, { env }); }
  catch (error) { fail(`${error.code ? `[${error.code}] ` : ""}${error.message}`); }
}

function fail(message) {
  console.error(`sign-updater: ${message}`);
  process.exit(1);
}
