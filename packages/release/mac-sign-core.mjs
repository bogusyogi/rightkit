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

const IDENTIFIER_PATTERN = /^[A-Za-z0-9][A-Za-z0-9.-]{0,254}$/;

/** A codesign `--identifier` value: reverse-DNS characters (letters, digits, `.`, `-`), no spaces, no leading dash. Returns it, or throws. */
export function validateCodesignIdentifier(identifier, label = "codesign identifier") {
  if (typeof identifier !== "string" || !IDENTIFIER_PATTERN.test(identifier)) {
    throw Object.assign(new Error(`${label} must be 1-255 characters of letters, digits, '.' and '-', starting with a letter or digit: ${JSON.stringify(identifier)}`), { code: "MAC_SIGN_IDENTIFIER_INVALID" });
  }
  return identifier;
}

/**
 * codesign argv for one entry. With no entitlements/options/identifier it is the historical `--force --options runtime --timestamp --sign <id> <file>`.
 * An optional `identifier` adds `--identifier <id>` between `--timestamp` and `--sign`.
 */
export function macCodesignArgs({ file, identity, entitlements, hardenedRuntime, options, identifier }) {
  const flags = codesignOptionsValue({ hardenedRuntime, options });
  if (identifier !== undefined) validateCodesignIdentifier(identifier);
  return ["--force", ...(flags ? ["--options", flags] : []), "--timestamp",
    ...(identifier !== undefined ? ["--identifier", identifier] : []), "--sign", identity,
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
    if (entry.identifier !== undefined) validateCodesignIdentifier(entry.identifier, `${label}: identifier`);
    return { file: entry.file, ...(entry.identifier !== undefined ? { identifier: entry.identifier } : {}), ...(entry.entitlements ? { entitlements: entry.entitlements } : {}), hardenedRuntime: entry.hardenedRuntime ?? true, options: entry.options ?? [] };
  });
}

const escapeRegExp = (value) => value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

/**
 * Sign, verify and record evidence for each entry. Throws Error(message) on the first failure.
 * Injectable: signOne(args) -> spawn-style result, inspect(cmd, args) -> output string (throws on failure),
 * evidence(file), hashFile(file).
 * Optional per-file identifiers: `identifiers` is a map from bundle-relative path (resolved against `bundle`; the bundle itself is `.`) to a codesign
 * identifier, or a function (relativePath, entry) -> identifier | undefined. An entry's own `identifier` wins; with neither,
 * no `--identifier` is passed. Every identifier is validated before anything is signed.
 */
export function signMacosEntries(entries, { identity, signOne, inspect, evidence = fileEvidence, hashFile = (file) => fileEvidence(file).sha256, identifiers, bundle }) {
  const planned = entries.map((entry) => {
    const relative = bundle ? path.relative(bundle, entry.file).split(path.sep).join("/") || "." : entry.file;
    const mapped = typeof identifiers === "function" ? identifiers(relative, entry) : identifiers ? Object.hasOwn(identifiers, relative) ? identifiers[relative] : undefined : undefined;
    const identifier = entry.identifier ?? mapped;
    if (identifier !== undefined) validateCodesignIdentifier(identifier, `identifier for ${relative}`);
    return identifier === undefined ? entry : { ...entry, identifier };
  });
  const records = [];
  for (const entry of planned) {
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
    if (entry.identifier !== undefined) {
      if (!details.includes(`Identifier=${entry.identifier}\n`)) throw new Error(`codesign identifier mismatch for ${entry.file}: expected ${entry.identifier}`);
      record.identifier = entry.identifier;
    }
    if (entry.entitlements) record.entitlements = { file: path.resolve(entry.entitlements), sha256: hashFile(entry.entitlements) };
    records.push(record);
  }
  return records;
}

export const readSignManifest = (file) => parseSignManifest(readFileSync(file, "utf8"));
