#!/usr/bin/env node
import { execFileSync, spawnSync } from "./background-process.mjs";
import { existsSync, readFileSync, rmSync } from "node:fs";
import os from "node:os";
import path from "node:path";

const args = process.argv.slice(2);
const files = [];
let dryRun = false;
for (let i = 0; i < args.length; i++) {
  if (args[i] === "--file") files.push(path.resolve(args[++i]));
  else if (args[i] === "--dry-run") dryRun = true;
  else fail(`unknown argument: ${args[i]}`);
}
if (!files.length) fail("at least one --file is required");

const env = { ...process.env };
if (!dryRun && !env.TAURI_SIGNING_PRIVATE_KEY) {
  if (process.platform === "win32") {
    const key = path.join(env.APPDATA || "", "ViewRight", "updater.key");
    if (!existsSync(key)) fail(`shared updater key not found: ${key}`);
    env.TAURI_SIGNING_PRIVATE_KEY = readFileSync(key, "utf8");
  } else if (process.platform === "darwin") {
    try {
      env.TAURI_SIGNING_PRIVATE_KEY = execFileSync(
        "security",
        ["find-generic-password", "-a", os.userInfo().username, "-s", "rightsuite-updater-key", "-w"],
        { encoding: "utf8" },
      ).trim();
    } catch {
      fail("shared updater key is missing from Keychain service rightsuite-updater-key");
    }
  } else {
    fail("TAURI_SIGNING_PRIVATE_KEY is required on this platform");
  }
}
env.TAURI_SIGNING_PRIVATE_KEY_PASSWORD ??= "";

for (const file of files) {
  if (!dryRun && !existsSync(file)) fail(`updater artifact not found: ${file}`);
  rmSync(`${file}.sig`, { force: true });
  console.log(`${dryRun ? "[dry-run] " : ""}pnpm exec tauri signer sign ${file}`);
  if (dryRun) continue;
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

function fail(message) {
  console.error(`sign-updater: ${message}`);
  process.exit(1);
}
