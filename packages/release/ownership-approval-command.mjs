#!/usr/bin/env node
// Draft/import/verify utilities. Signing is intentionally absent; native prompts own signing.
import { readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { createDraft, loadTrustedExternal, policyEntryDigest, validateRecord, verifyException } from "./ownership-approval.mjs";
import { SCANNER_VERSION } from "./ownership.mjs";
import manifest from "./rightkit-ownership.json" with { type: "json" };

function arg(args, name) { const i = args.indexOf(name); return i < 0 ? undefined : args[i + 1]; }
function json(file) { return JSON.parse(readFileSync(path.resolve(file), "utf8")); }
export function createInstalledDraft(input, { catalog = manifest, scannerVersion = SCANNER_VERSION, now = new Date() } = {}) {
  const draft = createDraft({ ...input, issued: now.toISOString().slice(0, 10), policyDigest: policyEntryDigest(catalog, input.fingerprint), minimumScannerVersion: scannerVersion, minimumCatalogVersion: catalog.version ?? String(catalog.schema) });
  const problems = validateRecord(draft);
  const duration = Date.parse(draft.payload.expires) - Date.parse(draft.payload.issued);
  if (!(duration > 0 && duration <= 90 * 86400000)) problems.push("draft expiry must be after issued and within 90 days");
  if (problems.length) throw new Error(problems.join("; "));
  return draft;
}
export function main(args) {
  if (args.includes("--draft")) {
    const input = json(arg(args, "--draft"));
    const draft = createInstalledDraft(input);
    const out = arg(args, "--out");
    const data = `${JSON.stringify(draft, null, 2)}\n`;
    if (out) writeFileSync(path.resolve(out), data, { flag: "wx" }); else process.stdout.write(data);
    return 0;
  }
  if (args.includes("--import")) {
    const record = json(arg(args, "--import"));
    const out = arg(args, "--out");
    if (!out) throw new Error("--import requires --out");
    writeFileSync(path.resolve(out), `${JSON.stringify(record, null, 2)}\n`, { flag: "wx" });
    return 0;
  }
  if (args.includes("--verify")) {
    const record = json(arg(args, "--verify"));
    const context = json(arg(args, "--context"));
    const loaded = loadTrustedExternal();
    const result = verifyException(record, { ...context, trust: loaded.trust });
    process.stdout.write(`${JSON.stringify({ ...result, pending: loaded.pending }, null, 2)}\n`);
    return result.ok ? 0 : 1;
  }
  throw new Error("usage: node ownership-approval-command.mjs --draft input.json [--out draft.json] | --import signed.json --out record.json | --verify signed.json --context scan.json");
}
if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try { process.exitCode = main(process.argv.slice(2)); }
  catch (error) { console.error(`[right-release] ownership approval: ${error.message}`); process.exitCode = 1; }
}
