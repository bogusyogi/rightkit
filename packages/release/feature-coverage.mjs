// Cargo feature coverage for the gate's Rust plan (rightkit#1231): feature-gated code is never left unchecked.
//
// Reported by RightKit's Pulse D15: pulse 69b649a3 passed the Dell pre-push gate with clippy::needless_return at windows/src/send.rs:2933
// under #[cfg(feature = "qa-views")]. A crate that declared no `featureSets` in ci/gate.config.json was linted with its default features
// only, and its QA-gated code was never linted.
//
//   1. `effectiveCrateFeatures`: a crate that declares no `featureSets` and whose Cargo.toml declares features beyond `default` is linted
//      twice, with its default features and with `--all-features` (the second also stands in for the `check-features` row, since clippy
//      type-checks everything `cargo check` does). A crate whose features conflict declares its sets explicitly.
//   2. `featureCoverageProblems`: every feature a crate's Cargo.toml declares must be enabled by at least one set of its plan (the
//      default features, a declared set, or a generated one); FEATURE_UNCOVERED names the crate and the feature.
//
//   3. `unlintedFeatures` (ci/gate.config.json rust.<crate>): a feature the gate never lints, named with its reason; it counts as covered, and
//      a name the crate does not declare, or that a set enables, is an error. Coverage is judged over every set whatever its hosts (feature-sets.mjs).
//
// Dependency-free by design (feature-sets.mjs imports nothing): scan-digest.mjs-style callers (the credentialed receipt verifiers) import it.
import { hasHostScopedSets, unlintedFeatures } from "./feature-sets.mjs";

/** The feature table of a Cargo.toml: a Map of feature name to its array of members ("other", "dep:x", "x/feat", "x?/feat"). `default` included. */
export function manifestFeatures(text) {
  const out = new Map();
  const src = String(text ?? "").replace(/^﻿/, "");
  let i = 0;
  const n = src.length;
  let inFeatures = false;
  // Walk the text once, string- and comment-aware, so a `[features]` mentioned in a string or comment, or a `#` inside a quoted member, is never misread.
  const skipLine = () => { while (i < n && src[i] !== "\n") i += 1; };
  const readString = () => {
    const quote = src[i]; i += 1; let value = "";
    while (i < n && src[i] !== quote && src[i] !== "\n") { if (quote === '"' && src[i] === "\\" && i + 1 < n) { value += src[i + 1]; i += 2; } else { value += src[i]; i += 1; } }
    if (src[i] === quote) i += 1;
    return value;
  };
  const skipSpace = () => { while (i < n && /\s/.test(src[i])) i += 1; };
  while (i < n) {
    skipSpace();
    if (i >= n) break;
    if (src[i] === "#") { skipLine(); continue; }
    if (src[i] === "[") {
      const double = src[i + 1] === "[";
      const close = src.indexOf("]", i);
      const header = close < 0 ? "" : src.slice(i + (double ? 2 : 1), close).trim().replace(/\s+/g, "");
      inFeatures = !double && header === "features";
      i = close < 0 ? n : close + (double ? 2 : 1);
      skipLine();
      continue;
    }
    // A key: bare or quoted, then `=`.
    let key = "";
    if (src[i] === '"' || src[i] === "'") key = readString();
    else { while (i < n && !/[\s=]/.test(src[i])) { key += src[i]; i += 1; } }
    skipSpace();
    if (src[i] !== "=") { skipLine(); continue; }
    i += 1; skipSpace();
    if (src[i] === "[") {
      i += 1; const members = [];
      for (;;) {
        skipSpace();
        if (i >= n) break;
        if (src[i] === "#") { skipLine(); continue; }
        if (src[i] === "]") { i += 1; break; }
        if (src[i] === '"' || src[i] === "'") members.push(readString());
        else i += 1; // a comma
      }
      if (inFeatures && key !== "") out.set(key, members);
      skipLine();
    } else {
      // Any other value (a string, a number, an inline table) is not a feature definition; read to the end of the line.
      if (src[i] === '"' || src[i] === "'") readString();
      skipLine();
    }
  }
  return out;
}

/** The feature names (never `default`) a Cargo.toml declares. */
export const declaredFeatureNames = (text) => [...manifestFeatures(text).keys()].filter((name) => name !== "default");

/** The features one cargo feature-flag set (`--all-features`, `--no-default-features`, `--features a,b`, `--features=a`, `-F a`) enables, closure included. */
export function featuresEnabledBy(features, set) {
  const names = new Set(features.keys());
  let all = false; let noDefault = false; const asked = [];
  const words = Array.isArray(set) ? set : [];
  for (let k = 0; k < words.length; k += 1) {
    const w = String(words[k]);
    if (w === "--all-features") all = true;
    else if (w === "--no-default-features") noDefault = true;
    else if (w === "--features" || w === "-F") { asked.push(...String(words[k + 1] ?? "").split(/[,\s]+/)); k += 1; }
    else if (w.startsWith("--features=")) asked.push(...w.slice("--features=".length).split(/[,\s]+/));
    else if (w.startsWith("-F") && w.length > 2) asked.push(...w.slice(2).split(/[,\s]+/));
  }
  if (all) return new Set(names);
  const enabled = new Set();
  const queue = [...asked.filter(Boolean)];
  if (!noDefault && names.has("default")) queue.push("default");
  while (queue.length) {
    const name = queue.pop();
    if (enabled.has(name)) continue;
    // `pkg/feature` and `pkg?/feature` name a dependency's feature; `dep:pkg` an optional dependency: none is one of this crate's own features.
    if (name.includes("/") || name.startsWith("dep:")) continue;
    enabled.add(name);
    for (const member of features.get(name) ?? []) queue.push(member);
  }
  return enabled;
}

/** A crate's features that none of `sets` enables and the crate does not list in `unlinted` (`unlintedFeatures`): `[]` when every declared feature is covered. */
export function uncoveredFeatures(manifestText, sets, unlinted = {}) {
  const features = manifestFeatures(manifestText);
  const declared = [...features.keys()].filter((name) => name !== "default" && !Object.hasOwn(unlinted, name));
  const covered = new Set();
  for (const set of sets) for (const f of featuresEnabledBy(features, set)) covered.add(f);
  return declared.filter((name) => !covered.has(name)).sort();
}

/**
 * The problem lines of a crate's `unlintedFeatures` (feature-sets.mjs): a feature it names must be declared by the crate's Cargo.toml and
 * enabled by none of the planned sets, or the entry is stale and the gate would lint a feature the config says it never lints.
 */
export function unlintedFeatureProblems({ name, manifestText, sets, unlinted = {} }) {
  const features = manifestFeatures(manifestText);
  const enabled = new Set();
  for (const set of sets) for (const f of featuresEnabledBy(features, set)) enabled.add(f);
  const problems = [];
  for (const feature of Object.keys(unlinted).sort()) {
    if (feature === "default" || !features.has(feature)) problems.push(`UNLINTED_FEATURE_UNKNOWN ${name}: unlintedFeatures names "${feature}", which the crate's Cargo.toml does not declare; remove it from ci/gate.config.json rust.${name}.unlintedFeatures`);
    else if (enabled.has(feature)) problems.push(`UNLINTED_FEATURE_ENABLED ${name}: unlintedFeatures names "${feature}" but a declared feature set enables it; it is linted, so remove it from ci/gate.config.json rust.${name}.unlintedFeatures`);
  }
  return problems;
}

/**
 * The FEATURE_UNCOVERED lines of one crate (`name`, its manifest text, every feature set its plan runs on any host, its `unlintedFeatures`),
 * and the lines of an `unlintedFeatures` entry that is stale.
 */
export function featureCoverageProblems({ name, manifestText, sets, unlinted = {} }) {
  const missing = uncoveredFeatures(manifestText, sets, unlinted);
  const planned = sets.map((s) => (s.length ? s.join(" ") : "default features")).join("; ");
  return [
    ...missing.map((feature) => `FEATURE_UNCOVERED ${name}: feature "${feature}" is enabled by none of the planned feature sets (${planned}); add a set that enables it to ci/gate.config.json rust.${name}.featureSets, or --all-features where the features do not conflict, or name it with a reason in rust.${name}.unlintedFeatures`),
    ...unlintedFeatureProblems({ name, manifestText, sets, unlinted }),
  ];
}

/**
 * `crateFeatures` (ci/gate.config.json `rust`) with the generated lint plan for one crate: when the crate declares no `featureSets` and its
 * Cargo.toml declares a feature beyond `default`, `autoAllFeatures: true` is set on its entry. rustCratePlan reads that marker (scan-digest.mjs).
 * `manifestOf(name)` returns the crate's Cargo.toml text or null. Pure.
 */
export function effectiveCrateFeatures({ crateFeatures = {}, units, manifestOf }) {
  const out = { ...crateFeatures };
  for (const name of units) {
    const own = crateFeatures?.[name] ?? {};
    // A malformed `unlintedFeatures` or host-scoped set is refused here, loudly, before any plan is built from it.
    const unlinted = Object.keys(unlintedFeatures(own, name));
    hasHostScopedSets(own, name);
    if (Array.isArray(own.featureSets) && own.featureSets.length) continue;
    let text = null;
    try { text = manifestOf(name); } catch { text = null; }
    if (typeof text !== "string") continue;
    const declared = declaredFeatureNames(text);
    if (!declared.length) continue;
    // A feature named in `unlintedFeatures` is never added to a generated set: the generated set lists every other feature instead of --all-features.
    if (unlinted.length) {
      const rest = declared.filter((feature) => !unlinted.includes(feature)).sort();
      if (rest.length) out[name] = { ...own, autoAllFeatures: true, autoFeatures: rest };
    } else out[name] = { ...own, autoAllFeatures: true };
  }
  return out;
}
