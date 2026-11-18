import { test } from "node:test";
import assert from "node:assert/strict";
import { allFeatureNamesOf } from "./feature-coverage.mjs";
import { rustCratePlan } from "./scan-digest.mjs";

const checkKept = (manifest) => rustCratePlan({}, undefined, "c", allFeatureNamesOf(manifest)).checkSets.some((s) => s.length === 1 && s[0] === "--all-features");

// Each expectation is the feature set Cargo gives the manifest, written from Cargo's rules (astra62's cases checked by astra62 with Python's tomllib), not from this reader.
test("NN27: --all-features turns on declared features and every optional dependency that is its own feature (astra62 reviews 1-3)", () => {
  const optional = (spec) => `[dependencies]\ncodec = ${spec}\n`;
  assert.deepEqual(allFeatureNamesOf(optional("{ version = \"1\", optional = true }")), ["codec"], "only an optional dependency");
  assert.equal(checkKept(optional("{ version = \"1\", optional = true }")), true, "so its check is kept");
  assert.deepEqual(allFeatureNamesOf("[features]\njson = [\"dep:codec\"]\n" + optional("{ version = \"1\", optional = true }")), ["json"], "dep: removes the implicit feature");
  assert.deepEqual(allFeatureNamesOf("# see \"dep:codec\"\n" + optional("{ version = \"1\", optional = true }")), ["codec"], "a comment is not a reference");
  assert.deepEqual(allFeatureNamesOf(optional("{ version = \"1\", \"optional\" = true }")), ["codec"], "quoted key");
  assert.deepEqual(allFeatureNamesOf(optional("{ version = \"1\", \"\\u006Fptional\" = true }")), ["codec"], "Unicode-escaped key");
  assert.deepEqual(allFeatureNamesOf("[\"features\"]\ncodec = []\n"), ["codec"], "quoted features table");
  assert.deepEqual(allFeatureNamesOf("[dependencies]\ncodec.version = \"1\"\ncodec.optional = true\n"), ["codec"], "dotted keys");
  assert.deepEqual(allFeatureNamesOf("[dependencies]\ncodec = {\n  version = \"1\",\n  optional = true,\n}\n".replace("{\n  version = \"1\",\n  optional = true,\n}", "{ version = \"1\", optional = true }")), ["codec"]);
  assert.deepEqual(allFeatureNamesOf("[features]\nfull = [\n  \"a\",\n  \"dep:codec\",\n]\na = []\n" + optional("{ version = \"1\", optional = true }")), ["full", "a"], "multi-line array");
  assert.deepEqual(allFeatureNamesOf("[dependencies.'codec']\nversion = \"1\"\noptional = true\n"), ["codec"], "quoted dependency table");
  assert.deepEqual(allFeatureNamesOf("[target.'cfg(unix)'.dependencies]\nnix = { version = \"0.29\", optional = true }\n[build-dependencies]\ncc = { version = \"1\", optional = true }\n").sort(), ["cc", "nix"], "target and build dependencies");
  assert.deepEqual(allFeatureNamesOf("[dependencies]\nort = { version = \"2\", optional = true }\n[target.'cfg(windows)'.dependencies]\nort = { version = \"2\", optional = true }\n"), ["ort"], "one dependency in several tables");
  // Cannot tell: null, and the check stays.
  assert.equal(allFeatureNamesOf("[features\nx = []\n"), null); assert.equal(checkKept("[features\nx = []\n"), true);
  assert.equal(allFeatureNamesOf("[features]\nx = \"not a list\"\n"), null);
  // A crate with no features and no optional dependency still drops its redundant check (NN27's intent).
  assert.deepEqual(allFeatureNamesOf("[package]\nname = \"c\"\n[dependencies]\nlibc = \"0.2\"\n"), []);
  assert.equal(checkKept("[package]\nname = \"c\"\n[dependencies]\nlibc = \"0.2\"\n"), false);
});

test("NN27: every consumer of a manifest's features reads the decoded table: lint generation, coverage and the check elision agree (astra62 review 4)", async () => {
  const { effectiveCrateFeatures, uncoveredFeatures, declaredFeatureNames } = await import("./feature-coverage.mjs");
  const quoted = "[package]\nname = \"c\"\n[\"features\"]\ncodec = []\n";
  assert.deepEqual(declaredFeatureNames(quoted), ["codec"]);
  assert.deepEqual(allFeatureNamesOf(quoted), ["codec"]);
  assert.equal(effectiveCrateFeatures({ units: ["c"], manifestOf: () => quoted }).c?.autoAllFeatures, true, "lint generation adds --all-features");
  assert.deepEqual(uncoveredFeatures(quoted, [[]]), ["codec"], "a default-only lint leaves codec uncovered, and coverage says so");
  // Text that does not parse is an explicit error, never a guess (astra62 review 6).
  assert.throws(() => declaredFeatureNames("[features]\na = []\n[broken\n"), /FEATURE_MANIFEST_MALFORMED/);
  // build_dependencies, the pre-2024 spelling.
  assert.deepEqual(allFeatureNamesOf("[build_dependencies]\ncc = { version = \"1\", optional = true }\n"), ["cc"]);
});

test("GC-A20: a crate whose features are implicit is clippy-linted with all features, in exactly one all-features pass, and coverage counts them (Adrian 2026-11-18)", async () => {
  const { effectiveCrateFeatures, uncoveredFeatures, featureCoverageProblems } = await import("./feature-coverage.mjs");
  const plan = (manifest, own = {}) => { const eff = effectiveCrateFeatures({ crateFeatures: { c: own }, units: ["c"], manifestOf: () => manifest }); return rustCratePlan(eff.c ?? {}, undefined, "c", allFeatureNamesOf(manifest)); };
  const allPasses = (p) => [...p.lintSets, ...p.checkSets].filter((s) => s.includes("--all-features")).length;
  // Implicit only (an optional build dependency, rightkit-audio's shape): clippy --all-features, no separate check.
  const implicitOnly = "[package]\nname = \"c\"\n[build_dependencies]\ncodec = { version = \"1\", optional = true }\n";
  const p1 = plan(implicitOnly);
  assert.ok(p1.lintSets.some((s) => s.includes("--all-features")), JSON.stringify(p1.lintSets));
  assert.equal(allPasses(p1), 1, "one all-features compiler pass");
  assert.deepEqual(uncoveredFeatures(implicitOnly, [[]]), ["codec"], "default-only lint leaves it uncovered");
  assert.deepEqual(uncoveredFeatures(implicitOnly, p1.lintSets), [], "the generated plan covers it");
  // Mixed explicit and implicit, with one unlinted feature: the generated set lists the rest by name, implicit included.
  const mixed = "[package]\nname = \"c\"\n[features]\njson = []\nslow = []\n[dependencies]\ncodec = { version = \"1\", optional = true }\n";
  const own = { unlintedFeatures: { slow: "needs hardware" } };
  const eff = effectiveCrateFeatures({ crateFeatures: { c: own }, units: ["c"], manifestOf: () => mixed });
  assert.deepEqual(eff.c.autoFeatures, ["codec", "json"]);
  const p2 = rustCratePlan(eff.c, undefined, "c", allFeatureNamesOf(mixed));
  assert.deepEqual(featureCoverageProblems({ name: "c", manifestText: mixed, sets: p2.lintSets, unlinted: own.unlintedFeatures }), []);
  // A crate with no features at all keeps its plain plan.
  const none = "[package]\nname = \"c\"\n[dependencies]\nlibc = \"0.2\"\n";
  assert.equal(plan(none).lintSets.some((s) => s.includes("--all-features")), false);
});

test("dependency features: a strong x/feat turns on x's implicit feature, a weak x?/feat does not, through feature closures, coverage and unlinted checks (astra62 review 6)", async () => {
  const { uncoveredFeatures, featureCoverageProblems, featuresEnabledBy, crateFeatureModel } = await import("./feature-coverage.mjs");
  const strong = "[package]\nname = \"c\"\n[features]\nexport = [\"codec/fast\"]\n[dependencies]\ncodec = { version = \"1\", optional = true }\n";
  assert.deepEqual(uncoveredFeatures(strong, [["--features", "export"]]), [], "export enables codec");
  const weak = strong.replace("codec/fast", "codec?/fast");
  assert.deepEqual(uncoveredFeatures(weak, [["--features", "export"]]), ["codec"], "a weak feature does not");
  // Transitive: full -> export -> codec/fast.
  const chain = strong.replace("[features]\n", "[features]\nfull = [\"export\"]\n");
  const m = crateFeatureModel(chain);
  assert.ok(featuresEnabledBy(m.features, ["--features", "full"], m.implicit).has("codec"));
  // dep:codec suppresses the implicit feature; codec/fast then enables only the dependency, no feature named codec exists.
  const suppressed = "[package]\nname = \"c\"\n[features]\nexport = [\"codec/fast\"]\nraw = [\"dep:codec\"]\n[dependencies]\ncodec = { version = \"1\", optional = true }\n";
  assert.equal(crateFeatureModel(suppressed).features.has("codec"), false);
  assert.deepEqual(uncoveredFeatures(suppressed, [["--features", "export,raw"]]), []);
  // An unlinted entry for a feature a strong dependency feature enables is stale.
  assert.ok(featureCoverageProblems({ name: "c", manifestText: strong, sets: [["--features", "export"]], unlinted: { codec: "slow" } }).some((p) => /UNLINTED_FEATURE_ENABLED c: .*"codec"/.test(p)));
  // Explicit diagnostics: malformed or missing manifests are problems, never an empty verdict; planning refuses them by crate.
  assert.match(featureCoverageProblems({ name: "c", manifestText: "[features\n", sets: [[]] }).join("\n"), /FEATURE_MANIFEST_MALFORMED: crate c Cargo.toml/);
  assert.match(featureCoverageProblems({ name: "c", manifestText: null, sets: [[]] }).join("\n"), /FEATURE_MANIFEST_MISSING: crate c Cargo.toml/);
  const { effectiveCrateFeatures } = await import("./feature-coverage.mjs");
  assert.throws(() => effectiveCrateFeatures({ units: ["c"], manifestOf: () => null }), /FEATURE_MANIFEST_MISSING: crate c/);
  assert.throws(() => effectiveCrateFeatures({ units: ["c"], manifestOf: () => { throw new Error("ENOENT"); } }), /FEATURE_MANIFEST_MISSING: crate c Cargo.toml could not be read \(ENOENT\)/);
});

test("a parser that cannot load is an explicit error naming the crate, not a fallback (astra62 review 6)", async () => {
  const fs = await import("node:fs"); const os = await import("node:os"); const path = await import("node:path"); const { pathToFileURL, fileURLToPath } = await import("node:url");
  const here = path.dirname(fileURLToPath(import.meta.url));
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "rk-no-toml-"));
  try {
    for (const f of ["feature-coverage.mjs", "feature-sets.mjs"]) fs.copyFileSync(path.join(here, f), path.join(dir, f));
    const mod = await import(pathToFileURL(path.join(dir, "feature-coverage.mjs")).href);
    assert.match(mod.featureCoverageProblems({ name: "c", manifestText: "[package]\n", sets: [[]] }).join("\n"), /FEATURE_MANIFEST_PARSER_MISSING: crate c Cargo.toml/);
    assert.throws(() => mod.effectiveCrateFeatures({ units: ["c"], manifestOf: () => "[package]\n" }), /FEATURE_MANIFEST_PARSER_MISSING/);
    assert.equal(mod.allFeatureNamesOf("[package]\n"), null, "and the NN27 elision drops no check");
  } finally { fs.rmSync(dir, { recursive: true, force: true }); }
});
