#!/usr/bin/env node
import { spawnSync } from "./background-process.mjs";
import { existsSync, rmSync } from "node:fs";
import path from "node:path";
import { signMacUpdaterFiles } from "./macos-signing-session.mjs";
import { ArtifactPathError, loadSplitArtifactContext, resolveArtifactPath, resolveArtifactWritePath } from "./release-artifact-path.mjs";

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

// A door split (row 7.04c, R-T2): a target-prefixed `--app` is the bound Cargo target's, an absolute one must lie inside it, and the tarball is
// written under the snapshot app root (a generated, ignored ledger entry). Outside a split both are resolved against the cwd as always.
let appPath;
let outputPath;
try {
  const artifactPaths = await loadSplitArtifactContext({ cwd });
  const split = artifactPaths.mode === "split";
  appPath = split ? resolveArtifactPath(app, artifactPaths) : path.resolve(cwd, app);
  outputPath = split ? resolveArtifactWritePath(output, artifactPaths) : path.resolve(cwd, output);
} catch (error) {
  if (error instanceof ArtifactPathError) fail(error.message);
  throw error;
}
if (!dryRun && !existsSync(appPath)) fail(`signed app not found: ${appPath}`);

const env = { ...process.env };
// bsdtar otherwise serializes macOS extended attributes as AppleDouble `._*`
// entries. Tauri's Rust extractor writes those entries as real bundle files,
// invalidating the Developer ID seal and causing syspolicyd to trash the app.
env.COPYFILE_DISABLE = "1";

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
if (dryRun) run("pnpm", ["exec", "tauri", "signer", "sign", outputPath]);
else {
  console.log(`pnpm exec tauri signer sign ${outputPath}`);
  try { signMacUpdaterFiles([outputPath], { cwd, env }); }
  catch (error) { fail(`${error.code ? `[${error.code}] ` : ""}${error.message}`); }
}

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
