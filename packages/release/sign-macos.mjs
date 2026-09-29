#!/usr/bin/env node
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, statSync, writeFileSync } from "node:fs";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";
import { MACOS_PORTABLE_SIGNING_CONTRACT } from "./macos-signing-contract.mjs";
import { resolveMacosDeveloperIdIdentity } from "./macos-signing-identity.mjs";

const args = process.argv.slice(2);
const dryRun = args.includes("--dry-run");
const receiptIndex = args.indexOf("--receipt");
const receiptPath = receiptIndex >= 0 ? args[receiptIndex + 1] : null;
const files = args
  .filter((arg, index) => arg !== "--dry-run" && arg !== "--receipt" && (receiptIndex < 0 || index !== receiptIndex + 1))
  .map((arg) => path.resolve(arg));

if (!files.length) fail("usage: node sign-macos.mjs [--dry-run] --receipt <path> <file>...");
if (!receiptPath) fail("--receipt requires a path");
if (dryRun) {
  console.log(`dry-run: Developer ID signing ${files.join(", ")}`);
  process.exit(0);
}
if (process.platform !== "darwin") fail("macOS signing must run on macOS");
let identity;
try { identity = resolveMacosDeveloperIdIdentity(); } catch (error) { fail(error.message); }
for (const file of files) if (!existsSync(file)) fail(`missing file: ${file}`);

const evidence = [];
for (const file of files) {
  const before = fileEvidence(file);
  run("codesign", ["--force", "--options", "runtime", "--timestamp", "--sign", identity, file]);
  const verification = run("codesign", ["--verify", "--strict", "--verbose=4", file]);
  const details = run("codesign", ["-dvv", file]);
  if (!/valid on disk/i.test(verification) || !/satisfies its designated requirement/i.test(verification)) {
    fail(`strict codesign verification failed for ${file}`);
  }
  if (!new RegExp(`^Authority=${escapeRegExp(identity)}$`, "m").test(details) && !details.includes(`Authority=${identity}`)) {
    fail(`codesign identity mismatch for ${file}: expected ${identity}`);
  }
  evidence.push({ file, before, after: fileEvidence(file), codesign: "strict-valid", identity, timestampPresent: /(?:Timestamp|Signed Time)[=:]/i.test(details) });
  if (!evidence.at(-1).timestampPresent) fail(`Developer ID timestamp is missing for ${file}`);
}

const absoluteReceipt = path.resolve(receiptPath);
mkdirSync(path.dirname(absoluteReceipt), { recursive: true });
writeFileSync(absoluteReceipt, `${JSON.stringify({ schema: 1, signingContract: MACOS_PORTABLE_SIGNING_CONTRACT, identity, files: evidence }, null, 2)}\n`);

function fileEvidence(file) {
  const bytes = readFileSync(file);
  return { sha256: createHash("sha256").update(bytes).digest("hex"), sizeBytes: statSync(file).size };
}

function run(cmd, runArgs) {
  const result = spawnSync(cmd, runArgs, { encoding: "utf8" });
  const output = `${result.stdout ?? ""}\n${result.stderr ?? ""}`;
  if (result.status !== 0) fail(`${cmd} ${runArgs.join(" ")} failed: ${output.trim()}`);
  return output;
}

function escapeRegExp(value) {
  return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

function fail(message) {
  console.error(`right-sign-macos: ${message}`);
  process.exit(1);
}
