#!/usr/bin/env node
import { existsSync } from "node:fs";
import path from "node:path";
import { notarizeMacosArtifact } from "./macos-notarization.mjs";

const args = process.argv.slice(2);
const dryRun = args.includes("--dry-run");
const receiptIndex = args.indexOf("--receipt");
const profileIndex = args.indexOf("--profile");
const appIndex = args.indexOf("--app");
const app = appIndex >= 0 ? args[appIndex + 1] : undefined;
const receiptPath = receiptIndex >= 0 ? args[receiptIndex + 1] : null;
const profile = profileIndex >= 0 ? args[profileIndex + 1] : undefined;
const archiveArgs = args.filter((arg, index) => arg !== "--dry-run" && arg !== "--receipt" && arg !== "--profile" && arg !== "--app" && (appIndex < 0 || index !== appIndex + 1) && (receiptIndex < 0 || index !== receiptIndex + 1) && (profileIndex < 0 || index !== profileIndex + 1));
if (archiveArgs.length !== 1) fail("usage: node notarize-macos.mjs [--dry-run] --receipt <path> [--profile <name>] [--app <bundle>] <archive>");
if (!receiptPath || receiptPath.startsWith("--")) fail("--receipt requires a path");
if (appIndex >= 0 && (!app || app.startsWith("--"))) fail("--app requires an app bundle path");
const archive = path.resolve(archiveArgs[0]);
if (dryRun) {
  console.log(`dry-run: notarize ${archive}`);
  process.exit(0);
}
if (process.platform !== "darwin") fail("macOS notarization must run on macOS");
if (!existsSync(archive)) fail(`missing archive: ${archive}`);

try { notarizeMacosArtifact({ archive, receiptPath, profile, app }); }
catch (error) {
  if (error.report) console.error(JSON.stringify(error.report));
  fail(`${error.code ? `[${error.code}] ` : ""}${error.message}`);
}

function fail(message) {
  console.error(`right-notarize-macos: ${message}`);
  process.exit(1);
}
