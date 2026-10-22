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

// ---- required scan ids ---------------------------------------------------------------------------------
// The one place the scan ids that belong to a crate are spelled, shared by planScans (deterministic-scans.mjs) and by the
// credentialed verifiers (scripts/ci/scan-receipt.mjs), so a verifier recomputes what a receipt must contain from the
// authenticated source and never trusts the plan the producer recorded. Dependency-free by design (see the header).

export const rustScanId = Object.freeze({
  check: (name) => `check:${name}`,
  checkFeatures: (name, index) => `check-features:${name}:${index}`,
  clippy: (name, count, index) => (count === 1 ? `clippy:${name}` : `clippy:${name}:${index}`),
  doc: (name) => `doc:${name}`,
  testRust: (name, count, index) => `test:rust:${name}${count === 1 ? "" : `:${index}`}`,
  testRustSerial: (name, count, index) => `test:rust-serial:${name}${count === 1 ? "" : `:${index}`}`,
  packageList: (name) => `cargo-package-list:${name}`,
  changelog: (name) => `changelog-version:${name}`,
});

/**
 * A crate's compile feature plan exactly as CI's gate runs it (ci/gate.config.json rust.<crate>): `lintSets` (clippy; default
 * features when none), whether a default `check` is needed (no lint set covers default features), and the `checkSets` that
 * remain (none when a lint set already uses --all-features).
 */
export function rustCratePlan(own = {}) {
  const lintSets = Array.isArray(own?.featureSets) && own.featureSets.length ? own.featureSets : [[]];
  const defaultCheck = !lintSets.some((set) => set.length === 0);
  const checkSets = (lintSets.some((set) => set.includes("--all-features")) ? [] : (Array.isArray(own?.checkFeatureSets) ? own.checkFeatureSets : [["--all-features"]])).filter((set) => set.length);
  return { lintSets, defaultCheck, checkSets };
}

/** Scan ids every scan of the whole repository always has when it holds Rust units (cargo.toml, workflows, YAML, JSON and licences). */
export const WORKSPACE_SCAN_IDS = Object.freeze(["cargo-metadata", "cargo-deny", "cargo-audit", "actionlint", "yaml-parse", "toml-parse", "json-parse", "license", "secret-scan"]);
/** Prefixes at least one scan id must carry (rustfmt groups by edition, so its exact ids depend on the files). */
export const REQUIRED_SCAN_PREFIXES = Object.freeze(["rustfmt:"]);

/**
 * The scan ids a LOCAL scan record must contain, each with status pass, for these publishable Rust `units` ([names], crates/<name>).
 * `exists(repoPath)` answers from the AUTHENTICATED source (never from the record): src/lib.rs decides `doc`, CHANGELOG.md decides
 * `changelog-version`, and the repository's own ci/advisory-exceptions.json, ownership catalog and docs generator decide theirs.
 * `crateFeatures` is ci/gate.config.json `rust`. `includeTests` adds the per-feature-set test scans. Extra scans in a record are
 * allowed; a missing one is not. planScans builds its ids from the same helpers, and a test holds the two in agreement.
 */
export function requiredScanIds({ units, crateFeatures = {}, exists, includeTests = false }) {
  if (!Array.isArray(units) || units.length === 0 || !units.every((n) => typeof n === "string" && n)) throw new TypeError("requiredScanIds: units must be a non-empty array of names");
  if (typeof exists !== "function") throw new TypeError("requiredScanIds: exists(repoPath) is required");
  const ids = new Set(WORKSPACE_SCAN_IDS);
  if (exists("ci/advisory-exceptions.json")) ids.add("advisory-exceptions");
  if (exists("packages/release/rightkit-ownership.json") && exists("crates/rightkit-ownership/catalog/ownership-catalog.json")) ids.add("ownership-catalog");
  if (exists("scripts/generate-index.mjs")) ids.add("generated-docs");
  for (const name of units) {
    const own = crateFeatures?.[name] ?? {};
    const { lintSets, defaultCheck, checkSets } = rustCratePlan(own);
    ids.add(rustScanId.packageList(name));
    if (exists(`crates/${name}/CHANGELOG.md`)) ids.add(rustScanId.changelog(name));
    if (defaultCheck) ids.add(rustScanId.check(name));
    checkSets.forEach((_set, i) => ids.add(rustScanId.checkFeatures(name, i)));
    lintSets.forEach((_set, i) => ids.add(rustScanId.clippy(name, lintSets.length, i)));
    if (exists(`crates/${name}/src/lib.rs`)) ids.add(rustScanId.doc(name));
    if (includeTests) {
      lintSets.forEach((_set, i) => {
        ids.add(rustScanId.testRust(name, lintSets.length, i));
        if (own.alsoSerial) ids.add(rustScanId.testRustSerial(name, lintSets.length, i));
      });
    }
  }
  return [...ids].sort();
}

/** Why `rows` (a record's scans) do not contain every required id with status pass; empty when they do. */
export function requiredScanProblems(rows, required, prefixes = REQUIRED_SCAN_PREFIXES) {
  const byId = new Map();
  for (const row of Array.isArray(rows) ? rows : []) if (row && typeof row.id === "string") byId.set(row.id, row);
  const problems = [];
  for (const id of required) {
    const row = byId.get(id);
    if (!row) problems.push(`required scan ${id} is missing`);
    else if (row.status !== "pass") problems.push(`required scan ${id} is ${row.status}`);
  }
  for (const prefix of prefixes) if (![...byId.values()].some((row) => row.id.startsWith(prefix) && row.status === "pass")) problems.push(`no passing ${prefix}* scan`);
  return problems;
}
