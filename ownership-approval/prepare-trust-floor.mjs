#!/usr/bin/env node
// Prepare a protected-trust candidate that moves the catalog/scanner floors to the committed
// catalog. Reads the installed trust (read-only), writes a candidate, prints its SHA-256 and the
// elevated install command. Installing remains an operator step through install-trust.mjs.
import { createHash } from "node:crypto";
import { readFileSync, realpathSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { trustedPath, validateTrustRegistry } from "../packages/release/ownership-approval.mjs";
import { catalogDigest, SCANNER_VERSION } from "../packages/release/ownership.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));

export function trustFloorCandidate(trust, manifest, scannerVersion = SCANNER_VERSION) {
  const candidate = { ...structuredClone(trust), minimumCatalogDigest: catalogDigest(manifest), minimumCatalogVersion: String(manifest.version), minimumScannerVersion: scannerVersion };
  const check = validateTrustRegistry(candidate);
  if (!check.ok) throw new Error(`candidate trust is invalid: ${check.problems.join("; ")}`);
  return candidate;
}

function main(args) {
  const out = args[args.indexOf("--out") + 1];
  if (!args.includes("--out") || !out) throw new Error("usage: node prepare-trust-floor.mjs --out <candidate.json>");
  const trust = JSON.parse(readFileSync(trustedPath(), "utf8"));
  const manifest = JSON.parse(readFileSync(path.join(here, "../packages/release/rightkit-ownership.json"), "utf8"));
  const bytes = JSON.stringify(trustFloorCandidate(trust, manifest), null, 2) + "\n";
  writeFileSync(out, bytes);
  const sha = createHash("sha256").update(bytes).digest("hex");
  const installer = path.join(here, "install-trust.mjs");
  process.stdout.write(`candidate: ${path.resolve(out)}\nsha256: ${sha}\n` + (process.platform === "win32"
    ? `elevated PowerShell: node "${installer}" --candidate "${path.resolve(out)}" --sha256 ${sha}\n`
    : `sudo node "${installer}" --candidate "${path.resolve(out)}" --sha256 ${sha}\n`));
}

const realOrSelf = (file) => { try { return realpathSync(file); } catch { return path.resolve(file); } };
if (process.argv[1] && realOrSelf(process.argv[1]) === realOrSelf(fileURLToPath(import.meta.url))) {
  try { main(process.argv.slice(2)); } catch (error) { process.stderr.write(`prepare-trust-floor: ${error.message}\n`); process.exitCode = 1; }
}
