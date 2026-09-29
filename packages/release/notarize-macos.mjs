#!/usr/bin/env node
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, statSync, writeFileSync } from "node:fs";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";
import { notarytoolAuthArgs } from "./notary-auth.mjs";
import { MACOS_PORTABLE_SIGNING_CONTRACT } from "./macos-signing-contract.mjs";

const args = process.argv.slice(2);
const dryRun = args.includes("--dry-run");
const receiptIndex = args.indexOf("--receipt");
const profileIndex = args.indexOf("--profile");
const receiptPath = receiptIndex >= 0 ? args[receiptIndex + 1] : null;
const profile = profileIndex >= 0 ? args[profileIndex + 1] : undefined;
const archiveArgs = args.filter((arg, index) => arg !== "--dry-run" && arg !== "--receipt" && arg !== "--profile" && (receiptIndex < 0 || index !== receiptIndex + 1) && (profileIndex < 0 || index !== profileIndex + 1));
if (archiveArgs.length !== 1) fail("usage: node notarize-macos.mjs [--dry-run] --receipt <path> [--profile <name>] <archive>");
if (!receiptPath) fail("--receipt requires a path");
const archive = path.resolve(archiveArgs[0]);
if (dryRun) {
  console.log(`dry-run: notarize ${archive}`);
  process.exit(0);
}
if (process.platform !== "darwin") fail("macOS notarization must run on macOS");
if (!existsSync(archive)) fail(`missing archive: ${archive}`);

let authArgs;
try { authArgs = notarytoolAuthArgs({ profile }); } catch (error) { fail(error.message); }
const output = run("xcrun", ["notarytool", "submit", archive, "--wait", "--output-format", "json", ...authArgs]);
let result;
try { result = JSON.parse(output); } catch { fail(`notarytool did not return JSON for ${archive}`); }
if (result.status !== "Accepted") fail(`notarization was not accepted for ${archive}: ${result.status ?? "missing status"}`);

const absoluteReceipt = path.resolve(receiptPath);
mkdirSync(path.dirname(absoluteReceipt), { recursive: true });
writeFileSync(absoluteReceipt, `${JSON.stringify({
  schema: 1,
  signingContract: MACOS_PORTABLE_SIGNING_CONTRACT,
  archive: { file: archive, ...fileEvidence(archive) },
  notarization: { status: result.status, id: result.id ?? result.submissionId ?? null },
}, null, 2)}\n`);

function fileEvidence(file) {
  const bytes = readFileSync(file);
  return { sha256: createHash("sha256").update(bytes).digest("hex"), sizeBytes: statSync(file).size };
}

function run(cmd, runArgs) {
  const result = spawnSync(cmd, runArgs, { encoding: "utf8" });
  const output = `${result.stdout ?? ""}\n${result.stderr ?? ""}`.trim();
  if (result.status !== 0) fail(`${cmd} ${runArgs.join(" ")} failed: ${output}`);
  return output;
}

function fail(message) {
  console.error(`right-notarize-macos: ${message}`);
  process.exit(1);
}
