// Signing loop for sign-macos.mjs, separated from the CLI so tests can drive it with a fake signer.
// One entry = one codesign invocation, in the given order (the caller orders inside-out).
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileEvidence } from "./macos-file-evidence.mjs";
import { CODESIGN_OPTION_FLAGS } from "./mac-signing-matrix.mjs";

export const SIGN_MANIFEST_SCHEMA = 1;

/** `--options` value: hardened runtime first (unless disabled), then the extra names. */
export function codesignOptionsValue({ hardenedRuntime = true, options = [] } = {}) {
  return [...(hardenedRuntime ? ["runtime"] : []), ...options.filter((item) => item !== "runtime")].join(",");
}

/** codesign argv for one entry. With no entitlements/options it is the historical `--force --options runtime --timestamp --sign <id> <file>`. */
export function macCodesignArgs({ file, identity, entitlements, hardenedRuntime, options }) {
  const flags = codesignOptionsValue({ hardenedRuntime, options });
  return ["--force", ...(flags ? ["--options", flags] : []), "--timestamp", "--sign", identity,
    ...(entitlements ? ["--entitlements", path.resolve(entitlements)] : []), file];
}

/** Parse the JSON manifest release.mjs writes for a signing step with per-file settings. */
export function parseSignManifest(text) {
  let manifest;
  try { manifest = JSON.parse(text); } catch { throw new Error("signing manifest is not valid JSON"); }
  if (manifest?.schema !== SIGN_MANIFEST_SCHEMA || !Array.isArray(manifest.entries) || !manifest.entries.length) throw new Error(`signing manifest must be { schema: ${SIGN_MANIFEST_SCHEMA}, entries: [...] } with at least one entry`);
  return manifest.entries.map((entry, index) => {
    const label = `signing manifest entry ${index}`;
    if (!entry || typeof entry.file !== "string" || !path.isAbsolute(entry.file)) throw new Error(`${label}: file must be an absolute path`);
    if (entry.entitlements !== undefined && (typeof entry.entitlements !== "string" || !path.isAbsolute(entry.entitlements))) throw new Error(`${label}: entitlements must be an absolute path`);
    if (entry.options !== undefined && (!Array.isArray(entry.options) || entry.options.some((item) => !CODESIGN_OPTION_FLAGS.includes(item)))) throw new Error(`${label}: options must be codesign option names (${CODESIGN_OPTION_FLAGS.join(", ")})`);
    if (entry.hardenedRuntime !== undefined && typeof entry.hardenedRuntime !== "boolean") throw new Error(`${label}: hardenedRuntime must be a boolean`);
    return { file: entry.file, ...(entry.entitlements ? { entitlements: entry.entitlements } : {}), hardenedRuntime: entry.hardenedRuntime ?? true, options: entry.options ?? [] };
  });
}

const escapeRegExp = (value) => value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

/**
 * Sign, verify and record evidence for each entry. Throws Error(message) on the first failure.
 * Injectable: signOne(args) -> spawn-style result, inspect(cmd, args) -> output string (throws on failure),
 * evidence(file), hashFile(file).
 */
export function signMacosEntries(entries, { identity, signOne, inspect, evidence = fileEvidence, hashFile = (file) => fileEvidence(file).sha256 }) {
  const records = [];
  for (const entry of entries) {
    const before = evidence(entry.file);
    const signed = signOne(macCodesignArgs({ ...entry, identity }));
    if (!signed || signed.error || signed.status !== 0) throw new Error(`codesign failed: ${signed?.stderr ?? signed?.error?.message ?? signed?.status}`);
    const verification = inspect("codesign", ["--verify", "--strict", "--verbose=4", entry.file]);
    const details = inspect("codesign", ["-dvv", entry.file]);
    if (!/valid on disk/i.test(verification) || !/satisfies its designated requirement/i.test(verification)) {
      throw new Error(`strict codesign verification failed for ${entry.file}`);
    }
    if (!new RegExp(`^Authority=${escapeRegExp(identity)}$`, "m").test(details) && !details.includes(`Authority=${identity}`)) {
      throw new Error(`codesign identity mismatch for ${entry.file}: expected ${identity}`);
    }
    const record = { file: entry.file, before, after: evidence(entry.file), codesign: "strict-valid", identity, timestampPresent: /(?:Timestamp|Signed Time)[=:]/i.test(details) };
    if (!record.timestampPresent) throw new Error(`Developer ID timestamp is missing for ${entry.file}`);
    record.codesignOptions = codesignOptionsValue(entry);
    if (entry.entitlements) record.entitlements = { file: path.resolve(entry.entitlements), sha256: hashFile(entry.entitlements) };
    records.push(record);
  }
  return records;
}

export const readSignManifest = (file) => parseSignManifest(readFileSync(file, "utf8"));
