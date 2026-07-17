#!/usr/bin/env node
import { execFileSync, spawnSync } from "node:child_process";
import { existsSync, rmSync } from "node:fs";
import os from "node:os";
import path from "node:path";

const args = process.argv.slice(2);
let app;
let output;
let cwd = process.cwd();
let dryRun = false;
for (let i = 0; i < args.length; i++) {
  if (args[i] === "--app") app = args[++i];
  else if (args[i] === "--output") output = args[++i];
  else if (args[i] === "--cwd") cwd = path.resolve(args[++i]);
  else if (args[i] === "--dry-run") dryRun = true;
  else fail(`unknown argument: ${args[i]}`);
}
if (!app || !output) fail("--app and --output are required");

const appPath = path.resolve(cwd, app);
const outputPath = path.resolve(cwd, output);
if (!dryRun && !existsSync(appPath)) fail(`signed app not found: ${appPath}`);

const env = { ...process.env };
// bsdtar otherwise serializes macOS extended attributes as AppleDouble `._*`
// entries. Tauri's Rust extractor writes those entries as real bundle files,
// invalidating the Developer ID seal and causing syspolicyd to trash the app.
env.COPYFILE_DISABLE = "1";
if (!dryRun && !env.TAURI_SIGNING_PRIVATE_KEY) {
  try {
    env.TAURI_SIGNING_PRIVATE_KEY = execFileSync(
      "security",
      ["find-generic-password", "-a", os.userInfo().username, "-s", "rightsuite-updater-key", "-w"],
      { encoding: "utf8" },
    ).trim();
  } catch {
    fail("shared updater key is missing from Keychain service rightsuite-updater-key");
  }
}
env.TAURI_SIGNING_PRIVATE_KEY_PASSWORD ??= "";

rmSync(outputPath, { force: true });
rmSync(`${outputPath}.sig`, { force: true });
// Tauri 2.10 strips the first path component while extracting. Omitting the explicit
// App.app/ directory entry prevents it from unpacking that entry onto its existing temp dir.
run("tar", [
  "-czf",
  outputPath,
  "-C",
  path.dirname(appPath),
  path.join(path.basename(appPath), "Contents"),
]);
run("pnpm", ["exec", "tauri", "signer", "sign", outputPath]);

function run(command, commandArgs) {
  const envPrefix = command === "tar" ? "COPYFILE_DISABLE=1 " : "";
  console.log(`${dryRun ? "[dry-run] " : ""}${envPrefix}${command} ${commandArgs.join(" ")}`);
  if (dryRun) return;
  const result = spawnSync(command, commandArgs, { cwd, env, stdio: "inherit" });
  if (result.status !== 0) process.exit(result.status ?? 1);
}

function fail(message) {
  console.error(`create-mac-updater: ${message}`);
  process.exit(1);
}
