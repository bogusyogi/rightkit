#!/usr/bin/env node
import { existsSync, mkdirSync, writeFileSync } from "node:fs";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";
import { readSignManifest, signMacosEntries } from "./mac-sign-core.mjs";
import { MACOS_PORTABLE_SIGNING_CONTRACT } from "./macos-signing-contract.mjs";
import { resolveMacosDeveloperIdIdentity } from "./macos-signing-identity.mjs";
import { runMacCodesignCommands } from "./macos-signing-session.mjs";

const args = process.argv.slice(2);
const dryRun = args.includes("--dry-run");
const receiptIndex = args.indexOf("--receipt");
const receiptPath = receiptIndex >= 0 ? args[receiptIndex + 1] : null;
const entitlementsIndex = args.indexOf("--entitlements");
const entitlementsPath = entitlementsIndex >= 0 ? args[entitlementsIndex + 1] : null;
const manifestIndex = args.indexOf("--manifest");
const manifestPath = manifestIndex >= 0 ? args[manifestIndex + 1] : null;
if (entitlementsIndex >= 0 && (!entitlementsPath || entitlementsPath.startsWith("--"))) fail("--entitlements requires a path");
if (manifestIndex >= 0 && (!manifestPath || manifestPath.startsWith("--"))) fail("--manifest requires a path");
const files = args
  .filter((arg, index) => arg !== "--dry-run" && arg !== "--receipt" && arg !== "--entitlements" && arg !== "--manifest"
    && (receiptIndex < 0 || index !== receiptIndex + 1)
    && (entitlementsIndex < 0 || index !== entitlementsIndex + 1)
    && (manifestIndex < 0 || index !== manifestIndex + 1))
  .map((arg) => path.resolve(arg));

// A manifest carries per-file settings (entitlements, codesign options) in signing order; the plain form
// signs every positional file with the same optional --entitlements.
if (manifestPath && (files.length || entitlementsPath)) fail("--manifest cannot be combined with positional files or --entitlements");
if (!manifestPath && !files.length) fail("usage: node sign-macos.mjs [--dry-run] [--entitlements <path>] --receipt <path> <file>... | --manifest <json>");
if (!receiptPath) fail("--receipt requires a path");
let entries;
if (manifestPath) {
  try { entries = readSignManifest(path.resolve(manifestPath)); } catch (error) { fail(error.message); }
} else {
  entries = files.map((file) => ({ file, ...(entitlementsPath ? { entitlements: path.resolve(entitlementsPath) } : {}), options: [] }));
}
if (dryRun) {
  console.log(`dry-run: Developer ID signing ${entries.map((entry) => entry.file).join(", ")}`);
  process.exit(0);
}
if (process.platform !== "darwin") fail("macOS signing must run on macOS");
let identity;
try { identity = resolveMacosDeveloperIdIdentity(); } catch (error) { fail(error.message); }
for (const entry of entries) if (!existsSync(entry.file)) fail(`missing file: ${entry.file}`);
for (const entry of entries) if (entry.entitlements && !existsSync(entry.entitlements)) fail(`missing entitlements: ${entry.entitlements}`);

let evidence;
try {
  evidence = signMacosEntries(entries, {
    identity,
    signOne: (codesignArgs) => runMacCodesignCommands([codesignArgs])[0],
    inspect: run,
  });
} catch (error) { fail(`${error.code ? `[${error.code}] ` : ""}${error.message}`); }

const absoluteReceipt = path.resolve(receiptPath);
mkdirSync(path.dirname(absoluteReceipt), { recursive: true });
writeFileSync(absoluteReceipt, `${JSON.stringify({ schema: 1, signingContract: MACOS_PORTABLE_SIGNING_CONTRACT, identity, files: evidence }, null, 2)}\n`);

function run(cmd, runArgs) {
  const result = spawnSync(cmd, runArgs, { encoding: "utf8" });
  const output = `${result.stdout ?? ""}\n${result.stderr ?? ""}`;
  if (result.status !== 0) fail(`${cmd} ${runArgs.join(" ")} failed: ${output.trim()}`);
  return output;
}

function fail(message) {
  console.error(`right-sign-macos: ${message}`);
  process.exit(1);
}
