// Canonical JSON and the scan-set digest, shared by the scanner (deterministic-scans.mjs) and the credentialed
// receipt verifiers (scripts/ci/scan-receipt.mjs). No imports beyond node:crypto, by design.
import crypto from "node:crypto";

const sha256 = (value) => crypto.createHash("sha256").update(value).digest("hex");

/** Deterministic JSON: object keys sorted, no undefined, no non-finite numbers. */
export function canonicalJson(value) {
  if (value === null || typeof value === "string" || typeof value === "boolean") return JSON.stringify(value);
  if (typeof value === "number") {
    if (!Number.isFinite(value)) throw new TypeError("canonicalJson: non-finite number");
    return JSON.stringify(value);
  }
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(",")}]`;
  if (typeof value === "object") {
    const keys = Object.keys(value).sort();
    for (const key of keys) if (value[key] === undefined) throw new TypeError(`canonicalJson: undefined at ${key}`);
    return `{${keys.map((key) => `${JSON.stringify(key)}:${canonicalJson(value[key])}`).join(",")}}`;
  }
  throw new TypeError(`canonicalJson: unsupported ${typeof value}`);
}

/** Digest of a scan set: the sorted (id, class, command) of every entry. Plan items and result rows agree. */
export function digestScanSet(entries) {
  const rows = entries.map((e) => [String(e.id), String(e.class), String(e.command)]).sort((a, b) => (a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : 0));
  return sha256(canonicalJson(rows));
}
