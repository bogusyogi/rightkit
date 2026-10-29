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

// ---- Clippy batching (GATE-CONTRACT §1 "Clippy route & batching"; item 8 rows 8.6, 8.8, 8.15) -----------------------------------------
// Lives here, beside the scan ids, so the credentialed verifiers (requiredScanIds) and the planner (planScans) spell a batch's id from
// one function. Pure: no file reads.

/** The feature flags that mean the same thing on every package they are applied to: any other set names features of one crate. */
export const CLIPPY_UNIVERSAL_FEATURE_FLAGS = Object.freeze(["--all-features", "--no-default-features"]);
const optionValue = (set, names) => {
  for (let i = 0; i < set.length; i += 1) for (const n of names) { if (set[i] === n) return set[i + 1] ?? null; if (set[i].startsWith(`${n}=`)) return set[i].slice(n.length + 1); }
  return null;
};
/** The target and profile a feature-set argv selects (null: the toolchain's host target and the dev profile). */
export const clippySelectionOf = (set) => ({ target: optionValue(set, ["--target"]), profile: set.includes("--release") || set.includes("-r") ? "release" : optionValue(set, ["--profile"]) });

/**
 * Groups the Clippy configurations of a plan into invocations. `entries`: `[{ name, crate, ws, set, count, env }]`, one per crate
 * and lint feature set (`count` is how many sets the crate declares). Entries are batched when workspace, feature set, target,
 * profile, lint policy, toolchain and environment are all equal (they share one `-p` list); an entry is its own invocation, with
 * the reason recorded, when:
 *  - its crate is in `solo` (ci/shards.json `solo`: a crate that always gets a job of its own);
 *  - it is in `separate` (a name to a reason): batching changed what compiles for it (feature unification, item 8 row 8.7);
 *  - its feature set names crate-specific features (anything but --all-features / --no-default-features): such a set is
 *    per-configuration by construction, for example rightkit-db's SQLCipher variants or rightkit-qa-driver's wv2 generations
 *    (ci/gate.config.json featureSets);
 *  - its crate is sidecar-bound: it is scheduled after every independent batch and never joins one.
 * A batch never widens a package set: it names exactly its members with `-p`. A batch's id is `clippy:batch:[<ws>:]<8 hex>` over its
 * workspace, feature set, target, profile, lint policy and member list (never the toolchain or the output directory, which a
 * verifier does not know), so the planner and requiredScanIds agree and the id names exactly the crates it covers. Returns
 * `{ batches, notes }`; batches are ordered independent first, then sidecar-bound, each group by workspace then key.
 */
export function clippyBatchPlan(entries, { solo = [], separate = new Map(), sidecarBound = new Set(), toolchain = null, lint = ["-D", "warnings"] } = {}) {
  const soloSet = new Set(solo);
  const groups = new Map();
  for (const e of entries) {
    const set = [...e.set];
    const crate = e.crate ?? e.name;
    const sel = clippySelectionOf(set);
    let reason = null;
    if (soloSet.has(e.name) || soloSet.has(crate)) reason = "ci/shards.json solo: the crate always gets an invocation of its own";
    else if (separate instanceof Map && separate.has(e.name)) reason = separate.get(e.name);
    else if (set.some((w) => !CLIPPY_UNIVERSAL_FEATURE_FLAGS.includes(w))) reason = `ci/gate.config.json featureSets names crate-specific features (${set.join(" ")})${e.count > 1 ? `; ${crate} declares ${e.count} feature configurations that are separate invocations by construction` : ""}`;
    const bound = sidecarBound.has(e.name) || sidecarBound.has(crate);
    const identity = { ws: e.ws, set, target: sel.target, profile: sel.profile, lint, ...(reason || bound ? { only: e.name, sidecar: bound } : {}) };
    const key = canonicalJson({ ...identity, toolchain, env: e.env ?? null });
    if (!groups.has(key)) groups.set(key, { key, identity: canonicalJson(identity), ws: e.ws, set, target: sel.target, profile: sel.profile, units: [], crates: [], reason: reason ?? (bound ? "sidecar-bound: scheduled after its sidecar prerequisite, never batched with independent crates" : null), order: bound ? "sidecar" : "independent" });
    const g = groups.get(key);
    g.units.push(e.name); if (!g.crates.includes(crate)) g.crates.push(crate);
  }
  const batches = [...groups.values()].map((g) => ({ ...g, units: g.units.sort(), crates: g.crates.sort() })).map((g) => ({ ...g, id: `clippy:batch:${g.ws === "." ? "" : `${g.ws}:`}${sha256(`${g.identity}\0${g.units.join("\0")}`).slice(0, 8)}` }));
  batches.sort((a, b) => (a.order === b.order ? (a.ws === b.ws ? (a.key < b.key ? -1 : 1) : a.ws < b.ws ? -1 : 1) : a.order === "independent" ? -1 : 1));
  const notes = batches.filter((b) => b.reason).map((b) => `clippy batching: ${b.units.join(", ")} (${b.set.join(" ") || "default features"}) stays its own invocation: ${b.reason}`);
  notes.push(`clippy batching: ${entries.length} crate configuration(s) in ${batches.length} invocation(s), each \`-p\` list exactly its members, \`--locked\` and \`--no-deps\` on every one; no batch is \`--workspace\``);
  return { batches, notes };
}

/** Scan ids every scan of the whole repository always has when it holds Rust units (cargo.toml, workflows, YAML, JSON and licences). */
export const WORKSPACE_SCAN_IDS = Object.freeze(["cargo-metadata", "cargo-deny", "cargo-audit", "actionlint", "yaml-parse", "toml-parse", "json-parse", "license", "secret-scan"]);
/** Prefixes at least one scan id must carry (rustfmt groups by edition, so its exact ids depend on the files). */
export const REQUIRED_SCAN_PREFIXES = Object.freeze(["rustfmt:"]);

/**
 * The scan ids a LOCAL scan record must contain, each with status pass, for these publishable Rust `units` ([names], crates/<name>).
 * `exists(repoPath)` answers from the AUTHENTICATED source (never from the record): src/lib.rs decides `doc`, CHANGELOG.md decides
 * `changelog-version`, and the repository's own ci/advisory-exceptions.json (or, per crate, crates/<name>/.rightkit/advisory-exceptions.json), ownership catalog and docs generator decide theirs.
 * `crateFeatures` is ci/gate.config.json `rust`. `includeTests` adds the per-feature-set test scans. Extra scans in a record are
 * allowed; a missing one is not. planScans builds its ids from the same helpers, and a test holds the two in agreement.
 */
export function requiredScanIds({ units, crateFeatures = {}, exists, includeTests = false, perCrateWorkspaces = undefined, clippyBatching = false, shardSolo = [], clippySeparate = new Map(), sidecarBound = new Set() }) {
  if (!Array.isArray(units) || units.length === 0 || !units.every((n) => typeof n === "string" && n)) throw new TypeError("requiredScanIds: units must be a non-empty array of names");
  if (typeof exists !== "function") throw new TypeError("requiredScanIds: exists(repoPath) is required");
  // One Cargo workspace and lock per crate (rightkit#1285): every unit has its own crates/<name>/Cargo.lock, so the per-workspace scans
  // (metadata, deny, audit and the advisory ledger where the crate has one) are required per unit and carry the crate name; there is no
  // repository-level ledger. A repository with one root workspace has no per-crate lock and keeps the unsuffixed ids.
  const perCrate = perCrateWorkspaces ?? units.every((n) => exists(`crates/${n}/Cargo.lock`));
  const ids = new Set(WORKSPACE_SCAN_IDS.filter((id) => !(perCrate && /^cargo-(metadata|deny|audit)$/.test(id))));
  if (!perCrate && exists("ci/advisory-exceptions.json")) ids.add("advisory-exceptions");
  if (exists("packages/release/rightkit-ownership.json") && exists("crates/rightkit-ownership/catalog/ownership-catalog.json")) ids.add("ownership-catalog");
  if (exists("scripts/generate-index.mjs")) ids.add("generated-docs");
  const clippyEntries = [];
  for (const name of units) {
    const own = crateFeatures?.[name] ?? {};
    const { lintSets, defaultCheck, checkSets } = rustCratePlan(own);
    if (perCrate) {
      for (const id of ["cargo-metadata", "cargo-deny", "cargo-audit"]) ids.add(`${id}:${name}`);
      if (exists(`crates/${name}/.rightkit/advisory-exceptions.json`)) ids.add(`advisory-exceptions:${name}`);
    }
    ids.add(rustScanId.packageList(name));
    if (exists(`crates/${name}/CHANGELOG.md`)) ids.add(rustScanId.changelog(name));
    if (defaultCheck) ids.add(rustScanId.check(name));
    checkSets.forEach((_set, i) => ids.add(rustScanId.checkFeatures(name, i)));
    if (clippyBatching) lintSets.forEach((set) => clippyEntries.push({ name, crate: name, ws: ".", set, count: lintSets.length, env: null }));
    else lintSets.forEach((_set, i) => ids.add(rustScanId.clippy(name, lintSets.length, i)));
    if (exists(`crates/${name}/src/lib.rs`)) ids.add(rustScanId.doc(name));
    if (includeTests) {
      lintSets.forEach((_set, i) => {
        ids.add(rustScanId.testRust(name, lintSets.length, i));
        if (own.alsoSerial) ids.add(rustScanId.testRustSerial(name, lintSets.length, i));
      });
    }
  }
  // With batching on, the Clippy ids are the batches' (the same function the planner calls), never the per-crate ones.
  if (clippyBatching) for (const b of clippyBatchPlan(clippyEntries, { solo: shardSolo, separate: clippySeparate, sidecarBound }).batches) ids.add(b.id);
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
