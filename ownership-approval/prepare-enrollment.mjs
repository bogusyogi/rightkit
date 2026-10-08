#!/usr/bin/env node
// Source-side candidate builder only. This file is not an authoritative installer.
import { realpathSync, readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { DEFAULT_TRUST_PATH } from "../packages/release/ownership-approval.mjs";
import { canonicalJson, keyIdForPublicKey, prepareTrustCandidate, sha256, validateEnrollmentExport } from "./trust-schema.mjs";

export { prepareTrustCandidate, validateEnrollmentExport };

function readJson(file) { return JSON.parse(readFileSync(file, "utf8")); }

function candidateOutputPath(file) {
  const requested = path.resolve(file);
  const parent = realpathSync(path.dirname(requested));
  const output = path.join(parent, path.basename(requested));
  const trustFile = DEFAULT_TRUST_PATH[process.platform];
  if (trustFile) {
    const trustDir = path.dirname(path.resolve(trustFile));
    const compare = process.platform === "win32" ? (v) => v.toLowerCase() : (v) => v;
    const target = compare(output), protectedDir = compare(trustDir + path.sep);
    if (target === compare(path.resolve(trustFile)) || target.startsWith(protectedDir)) {
      throw new Error("candidate output may not target the fixed OS trust location");
    }
  }
  return output;
}

function main(args) {
  if (args.length !== 8 || args[0] !== "--baseline" || args[2] !== "--windows" || args[4] !== "--macos" || args[6] !== "--out") {
    throw new Error("usage: node prepare-enrollment.mjs --baseline <admin-trust.json> --windows <windows-public.json> --macos <macos-public.json> --out <candidate.json>");
  }
  const [, baselinePath, , windowsPath, , macosPath, , outPath] = args;
  const candidate = prepareTrustCandidate({
    baseline: readJson(baselinePath),
    windowsExport: readJson(windowsPath),
    macosExport: readJson(macosPath),
  });
  const output = candidateOutputPath(outPath);
  const bytes = Buffer.from(`${JSON.stringify(candidate, null, 2)}\n`, "utf8");
  writeFileSync(output, bytes, { flag: "wx", mode: 0o600 });
  process.stdout.write(`Prepared non-authoritative candidate: ${output}\n`);
  process.stdout.write(`Candidate file SHA-256 (pin this before install): ${sha256(bytes)}\n`);
  process.stdout.write(`Canonical trust SHA-256: ${sha256(canonicalJson(candidate))}\n`);
  process.stdout.write(`Windows public-key fingerprint: ${candidate.hosts.windows.keyId}\n`);
  process.stdout.write(`macOS public-key fingerprint: ${candidate.hosts.macos.keyId}\n`);
}

const realOrSelf = (file) => { try { return realpathSync(file); } catch { return path.resolve(file); } };
if (process.argv[1] && realOrSelf(process.argv[1]) === realOrSelf(fileURLToPath(import.meta.url))) {
  try { main(process.argv.slice(2)); }
  catch (error) { process.stderr.write(`Enrollment candidate rejected: ${error.message}\n`); process.exitCode = 1; }
}
