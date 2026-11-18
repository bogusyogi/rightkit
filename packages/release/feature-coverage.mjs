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
// Reads manifests with smol-toml, loaded on first parse, so importing this module (scan-digest.mjs-style callers, the credentialed receipt verifiers) needs no install.
import { hasHostScopedSets, unlintedFeatures } from "./feature-sets.mjs";
import { createRequire } from "node:module";

/**
 * A manifest the feature readers cannot read. `kind`: "parser-missing" (smol-toml cannot load), "malformed" (not valid TOML, or a [features] entry
 * that is not a list of strings), "missing" (no manifest text). Planning and coverage never fall back to a guess: a plan cannot be built from it and
 * coverage reports it as a problem (astra62 review 6, 2026-11-18; the old text walk is gone).
 */
export class FeatureManifestError extends Error {
  constructor(kind, detail) { super(`FEATURE_MANIFEST_${kind.toUpperCase().replace(/-/g, "_")}: ${detail}`); this.name = "FeatureManifestError"; this.kind = kind; }
}

let smolTomlParse;
/** The decoded manifest (smol-toml, loaded on first use as in cargo-contract.mjs, so importing this module needs no install). Throws FeatureManifestError. */
export function decodedManifest(text, where = "Cargo.toml") {
  if (typeof text !== "string") throw new FeatureManifestError("missing", `${where} could not be read`);
  let parse;
  try { parse = (smolTomlParse ??= createRequire(import.meta.url)("smol-toml").parse); } catch (e) { throw new FeatureManifestError("parser-missing", `${where}: smol-toml cannot be loaded (${e.message}); install @rightkit/release's dependencies where the gate runs`); }
  try { return parse(text.replace(/^﻿/, "")); } catch (e) { throw new FeatureManifestError("malformed", `${where} is not valid TOML: ${String(e?.message ?? e).split("\n")[0]}`); }
}

const isTable = (v) => v !== null && typeof v === "object" && !Array.isArray(v);

function featuresOfDoc(doc, where) {
  const table = doc.features === undefined ? {} : doc.features;
  if (!isTable(table)) throw new FeatureManifestError("malformed", `${where}: [features] is not a table`);
  const out = new Map();
  for (const [name, members] of Object.entries(table)) {
    if (!Array.isArray(members) || members.some((m) => typeof m !== "string")) throw new FeatureManifestError("malformed", `${where}: feature "${name}" is not a list of strings`);
    out.set(name, [...members]);
  }
  return out;
}

// Dev-dependencies cannot be optional (Cargo refuses it). build_dependencies is the spelling Cargo accepted before edition 2024.
function optionalDependenciesOfDoc(doc, where) {
  const tables = [doc.dependencies, doc["build-dependencies"], doc.build_dependencies, ...(isTable(doc.target) ? Object.values(doc.target).flatMap((t) => (isTable(t) ? [t.dependencies, t["build-dependencies"], t.build_dependencies] : [null])) : [])];
  const optional = new Set();
  for (const t of tables) {
    if (t === undefined) continue;
    if (!isTable(t)) throw new FeatureManifestError("malformed", `${where}: a dependency table is not a table`);
    for (const [name, spec] of Object.entries(t)) if (isTable(spec) && spec.optional === true) optional.add(name);
  }
  return optional;
}

/** The [features] table of a Cargo.toml: a Map of feature name to its members ("other", "dep:x", "x/feat", "x?/feat"), `default` included. Throws FeatureManifestError. */
export function manifestFeatures(text, where = "Cargo.toml") { return featuresOfDoc(decodedManifest(text, where), where); }

/** The feature names (never `default`) a Cargo.toml's [features] table declares. Throws FeatureManifestError. */
export const declaredFeatureNames = (text, where) => [...manifestFeatures(text, where).keys()].filter((name) => name !== "default");

/**
 * The features Cargo gives a crate, from its decoded manifest: the [features] table plus each optional dependency that is its own implicit feature
 * (Cargo gives an optional dependency a feature of its name unless a feature names it as `dep:<name>`), kept as `name = ["dep:name"]` and listed in
 * `implicit`. Lint generation, feature coverage, unlinted validation and the NN27 check elision all read this one model (GC-A20; Adrian 2026-11-18).
 * Throws FeatureManifestError.
 */
export function crateFeatureModel(text, where = "Cargo.toml") {
  const doc = decodedManifest(text, where);
  const features = featuresOfDoc(doc, where);
  const referenced = new Set([...features.values()].flat().filter((v) => v.startsWith("dep:")).map((v) => v.slice(4)));
  const implicit = new Set([...optionalDependenciesOfDoc(doc, where)].filter((name) => !referenced.has(name) && !features.has(name)));
  for (const name of implicit) features.set(name, [`dep:${name}`]);
  return { features, implicit };
}

/**
 * NN27: every feature `--all-features` turns on for this crate (crateFeatureModel, `default` left out), or null when the manifest cannot be read;
 * a null drops no check.
 */
export function allFeatureNamesOf(text) {
  try { return [...crateFeatureModel(text).features.keys()].filter((name) => name !== "default"); } catch { return null; }
}

/**
 * The features one cargo feature-flag set (`--all-features`, `--no-default-features`, `--features a,b`, `--features=a`, `-F a`) enables, closure included.
 * `implicit`: the crate's implicit optional-dependency features (crateFeatureModel). A strong dependency feature `x/feat` turns on the optional
 * dependency x and with it its implicit feature x; a weak one `x?/feat` does not; `dep:x` never names a feature
 * (https://doc.rust-lang.org/cargo/reference/features.html#dependency-features; astra62 review 6).
 */
export function featuresEnabledBy(features, set, implicit = new Set()) {
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
    if (enabled.has(name) || name.startsWith("dep:")) continue;
    if (name.includes("/")) {
      const dep = name.slice(0, name.indexOf("/"));
      if (!dep.endsWith("?") && implicit.has(dep)) queue.push(dep);
      continue;
    }
    enabled.add(name);
    for (const member of features.get(name) ?? []) queue.push(member);
  }
  return enabled;
}

/** A crate's features that none of `sets` enables and the crate does not list in `unlinted` (`unlintedFeatures`): `[]` when every feature, implicit ones included, is covered. Throws FeatureManifestError. */
export function uncoveredFeatures(manifestText, sets, unlinted = {}, where = "Cargo.toml") {
  const { features, implicit } = crateFeatureModel(manifestText, where);
  const declared = [...features.keys()].filter((name) => name !== "default" && !Object.hasOwn(unlinted, name));
  const covered = new Set();
  for (const set of sets) for (const f of featuresEnabledBy(features, set, implicit)) covered.add(f);
  return declared.filter((name) => !covered.has(name)).sort();
}

/**
 * The problem lines of a crate's `unlintedFeatures` (feature-sets.mjs): a feature it names must be one of the crate's features and enabled by none of
 * the planned sets, or the entry is stale and the gate would lint a feature the config says it never lints. Throws FeatureManifestError.
 */
export function unlintedFeatureProblems({ name, manifestText, sets, unlinted = {} }) {
  const { features, implicit } = crateFeatureModel(manifestText, `crate ${name} Cargo.toml`);
  const enabled = new Set();
  for (const set of sets) for (const f of featuresEnabledBy(features, set, implicit)) enabled.add(f);
  const problems = [];
  for (const feature of Object.keys(unlinted).sort()) {
    if (feature === "default" || !features.has(feature)) problems.push(`UNLINTED_FEATURE_UNKNOWN ${name}: unlintedFeatures names "${feature}", which the crate's Cargo.toml does not declare; remove it from ci/gate.config.json rust.${name}.unlintedFeatures`);
    else if (enabled.has(feature)) problems.push(`UNLINTED_FEATURE_ENABLED ${name}: unlintedFeatures names "${feature}" but a declared feature set enables it; it is linted, so remove it from ci/gate.config.json rust.${name}.unlintedFeatures`);
  }
  return problems;
}

/**
 * The FEATURE_UNCOVERED lines of one crate (`name`, its manifest text, every feature set its plan runs on any host, its `unlintedFeatures`),
 * and the lines of an `unlintedFeatures` entry that is stale. A manifest that cannot be read is itself a problem line, never an empty answer.
 */
export function featureCoverageProblems({ name, manifestText, sets, unlinted = {} }) {
  try {
    const missing = uncoveredFeatures(manifestText, sets, unlinted, `crate ${name} Cargo.toml`);
    const planned = sets.map((s) => (s.length ? s.join(" ") : "default features")).join("; ");
    return [
      ...missing.map((feature) => `FEATURE_UNCOVERED ${name}: feature "${feature}" is enabled by none of the planned feature sets (${planned}); add a set that enables it to ci/gate.config.json rust.${name}.featureSets, or --all-features where the features do not conflict, or name it with a reason in rust.${name}.unlintedFeatures`),
      ...unlintedFeatureProblems({ name, manifestText, sets, unlinted }),
    ];
  } catch (error) {
    if (error instanceof FeatureManifestError) return [`${error.message}; feature coverage of ${name} cannot be judged`];
    throw error;
  }
}

/**
 * `crateFeatures` (ci/gate.config.json `rust`) with the generated lint plan for one crate: when the crate declares no `featureSets` and has a
 * feature beyond `default` (declared or implicit, crateFeatureModel), `autoAllFeatures: true` is set on its entry. rustCratePlan reads that marker
 * (scan-digest.mjs). `manifestOf(name)` returns the crate's Cargo.toml text. A manifest that cannot be read throws FeatureManifestError naming the
 * crate: no plan is built from a guess. Pure.
 */
export function effectiveCrateFeatures({ crateFeatures = {}, units, manifestOf }) {
  const out = { ...crateFeatures };
  for (const name of units) {
    const own = crateFeatures?.[name] ?? {};
    // A malformed `unlintedFeatures` or host-scoped set is refused here, loudly, before any plan is built from it.
    const unlinted = Object.keys(unlintedFeatures(own, name));
    hasHostScopedSets(own, name);
    if (Array.isArray(own.featureSets) && own.featureSets.length) continue;
    let text;
    try { text = manifestOf(name); } catch (error) { throw new FeatureManifestError("missing", `crate ${name} Cargo.toml could not be read (${error?.message ?? error})`); }
    const { features } = crateFeatureModel(text, `crate ${name} Cargo.toml`);
    const declared = [...features.keys()].filter((feature) => feature !== "default");
    if (!declared.length) continue;
    // A feature named in `unlintedFeatures` is never added to a generated set: the generated set lists every other feature instead of --all-features.
    if (unlinted.length) {
      const rest = declared.filter((feature) => !unlinted.includes(feature)).sort();
      if (rest.length) out[name] = { ...own, autoAllFeatures: true, autoFeatures: rest };
    } else out[name] = { ...own, autoAllFeatures: true };
  }
  return out;
}
