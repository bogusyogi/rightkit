// Host-scoped feature sets and `unlintedFeatures` (ci/gate.config.json rust.<crate>; feature-sets.mjs; docs/GATE-CONTRACT.md "Feature sets").
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { CROSS_TARGETS } from "./cross-targets.mjs";
import { planScans, requiredScanIds } from "./deterministic-scans.mjs";
import { effectiveCrateFeatures, featureCoverageProblems, uncoveredFeatures } from "./feature-coverage.mjs";
import { FEATURE_SET_HOSTS, allFeatureSets, crossFeatureSets, defaultCheckFeatureSets, featureSetEntries, hasHostScopedSets, hostFeatureSets, unlintedFeatures } from "./feature-sets.mjs";
import { batchClippyInvocations, localTestPlan } from "./local-publish.mjs";
import { batchClippyMissingForHost } from "./publication-gate.mjs";
import { rustCoverageSets, rustCratePlan } from "./scan-digest.mjs";

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");
const HOSTS = ["darwin", "win32", "linux"];
const scratch = ((root) => (fs.existsSync(root) ? root : os.tmpdir()))(process.platform === "win32" ? "D:\\scratch" : "/Volumes/D/scratch");

const SCOPED = {
  platforms: ["darwin", "win32"], platformsReason: "fixture",
  featureSets: [
    [],
    { features: ["--features", "metal"], hosts: ["darwin"], hostsReason: "metal is Apple-only" },
    { features: ["--features", "vulkan"], hosts: ["win32"], hostsReason: "vulkan needs the Vulkan SDK" },
  ],
  checkFeatureSets: [[]],
  unlintedFeatures: { regen: "rewrites checked-in files" },
};
const MANIFEST = `[package]\nname = "fx"\nversion = "0.1.0"\n[features]\ndefault = []\nmetal = []\nvulkan = []\nnative = []\nregen = []\n`;

test("FEATURE_SET_HOSTS is the host vocabulary of crate-level platforms (CROSS_TARGETS)", () => {
  assert.deepEqual([...FEATURE_SET_HOSTS].sort(), Object.keys(CROSS_TARGETS).sort());
});

test("normaliser: a list with no object entry is returned as the very same array, so every existing row and id is unchanged", () => {
  const entry = { featureSets: [[], ["--features", "a"]], checkFeatureSets: [["--all-features"]] };
  for (const platform of HOSTS) {
    assert.equal(hostFeatureSets(entry, platform), entry.featureSets);
    assert.equal(crossFeatureSets(entry, platform), entry.featureSets);
    assert.equal(allFeatureSets(entry), entry.featureSets);
    assert.equal(hostFeatureSets(entry, platform, "checkFeatureSets"), entry.checkFeatureSets);
  }
  assert.equal(hostFeatureSets({}, "darwin"), undefined);
  assert.equal(hasHostScopedSets(entry), false);
});

test("normaliser: object entries are filtered by host; scoped sets have no cross-target rows; coverage sees every set", () => {
  assert.deepEqual(hostFeatureSets(SCOPED, "darwin"), [[], ["--features", "metal"]]);
  assert.deepEqual(hostFeatureSets(SCOPED, "win32"), [[], ["--features", "vulkan"]]);
  assert.deepEqual(hostFeatureSets(SCOPED, "linux"), [[]]);
  for (const platform of HOSTS) assert.deepEqual(crossFeatureSets(SCOPED, platform), [[]]);
  assert.deepEqual(allFeatureSets(SCOPED), [[], ["--features", "metal"], ["--features", "vulkan"]]);
  assert.equal(hasHostScopedSets(SCOPED), true);
  assert.throws(() => hostFeatureSets(SCOPED, undefined, "featureSets", "fx"), /rust\.fx\.featureSets has host-scoped sets: the host's platform/);
});

test("normaliser: a malformed entry is refused loudly and names the crate", () => {
  const bad = (entry, pattern) => assert.throws(() => hostFeatureSets({ featureSets: [entry] }, "darwin", "featureSets", "fx-crate"), pattern);
  const good = { features: ["--features", "a"], hosts: ["darwin"], hostsReason: "why" };
  assert.deepEqual(featureSetEntries([good], "featureSets", "fx-crate"), [{ features: ["--features", "a"], hosts: ["darwin"] }]);
  bad("--features a", /rust\.fx-crate\.featureSets\[0\] must be an argv array or a \{ features, hosts, hostsReason \} object/);
  bad({ ...good, extra: 1 }, /rust\.fx-crate\.featureSets\[0\] has unknown key\(s\) extra/);
  bad({ ...good, features: "a" }, /rust\.fx-crate\.featureSets\[0\]\.features must be an argv array/);
  bad({ ...good, features: [""] }, /features must be an argv array of non-empty strings/);
  bad({ ...good, hosts: [] }, /rust\.fx-crate\.featureSets\[0\]\.hosts must be a non-empty list of distinct hosts from darwin, win32, linux/);
  bad({ ...good, hosts: undefined }, /\.hosts must be a non-empty list/);
  bad({ ...good, hosts: ["macos"] }, /\.hosts must be a non-empty list of distinct hosts/);
  bad({ ...good, hosts: ["darwin", "darwin"] }, /distinct hosts/);
  bad({ ...good, hostsReason: "  " }, /rust\.fx-crate\.featureSets\[0\] needs a non-empty hostsReason/);
  bad({ features: ["--features", "a"], hosts: ["darwin"] }, /needs a non-empty hostsReason/);
  assert.throws(() => allFeatureSets({ checkFeatureSets: [{ ...good, hosts: [] }] }, "checkFeatureSets", "fx-crate"), /rust\.fx-crate\.checkFeatureSets\[0\]\.hosts/);
  assert.throws(() => hasHostScopedSets({ featureSets: [[], { ...good, hostsReason: "" }] }, "fx-crate"), /fx-crate/);
});

test("unlintedFeatures: reasons are required; a crate that names any gets no --all-features default check", () => {
  assert.deepEqual(unlintedFeatures({}, "fx"), {});
  assert.deepEqual(unlintedFeatures({ unlintedFeatures: { regen: "why" } }, "fx"), { regen: "why" });
  assert.throws(() => unlintedFeatures({ unlintedFeatures: { regen: "" } }, "fx"), /rust\.fx\.unlintedFeatures\.regen needs a non-empty reason/);
  assert.throws(() => unlintedFeatures({ unlintedFeatures: ["regen"] }, "fx"), /rust\.fx\.unlintedFeatures must be an object/);
  assert.throws(() => unlintedFeatures({ unlintedFeatures: {} }, "fx"), /must be an object/);
  assert.deepEqual(defaultCheckFeatureSets({}, "fx"), [["--all-features"]]);
  assert.deepEqual(defaultCheckFeatureSets({ unlintedFeatures: { regen: "why" } }, "fx"), []);
});

// ---- planning ------------------------------------------------------------------------------------------------------------------

function fixture(names = ["fx"]) {
  fs.mkdirSync(scratch, { recursive: true });
  const root = fs.mkdtempSync(path.join(scratch, "rk-featuresets-"));
  for (const name of names) {
    fs.mkdirSync(path.join(root, "crates", name, "src"), { recursive: true });
    fs.writeFileSync(path.join(root, "crates", name, "Cargo.toml"), MANIFEST.replace('"fx"', JSON.stringify(name)));
    fs.writeFileSync(path.join(root, "crates", name, "src", "lib.rs"), "");
    fs.writeFileSync(path.join(root, "crates", name, "Cargo.lock"), "");
  }
  return root;
}
const plan = (root, platform, crateFeatures, names = ["fx"]) => planScans({
  changed: names.map((n) => `crates/${n}/Cargo.toml`), contentRoot: root, platform,
  rustUnits: names.map((n) => ({ name: n, dir: `crates/${n}`, cargoDir: `crates/${n}` })), npmUnits: [], crateFeatures, includeTests: true,
});
const rowIds = (result, name = "fx") => result.scans.map((s) => s.id).filter((id) => id.includes(`:${name}`) && /^(check|clippy|doc|test)/.test(id));
const featuresOfRow = (result, id) => {
  const args = result.scans.find((s) => s.id === id).args;
  const at = args.indexOf("--features");
  return at < 0 ? "default" : args[at + 1];
};

test("planning: a host plans only the sets that name it, and a host-scoped set has no cross-target row", (t) => {
  const root = fixture();
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const darwin = plan(root, "darwin", { fx: SCOPED });
  const win = plan(root, "win32", { fx: SCOPED });
  const linux = plan(root, "linux", { fx: SCOPED });
  assert.deepEqual(rowIds(darwin).filter((id) => !id.includes("@")), ["clippy:fx:0", "clippy:fx:1", "doc:fx", "test:rust:fx:0", "test:rust:fx:1"]);
  assert.equal(featuresOfRow(darwin, "clippy:fx:1"), "metal");
  assert.equal(featuresOfRow(darwin, "test:rust:fx:1"), "metal");
  assert.deepEqual(rowIds(win).filter((id) => !id.includes("@")), ["clippy:fx:0", "clippy:fx:1", "doc:fx", "test:rust:fx:0", "test:rust:fx:1"]);
  assert.equal(featuresOfRow(win, "clippy:fx:1"), "vulkan");
  assert.deepEqual(rowIds(linux), [], "the crate is scoped to darwin and win32 by platforms: no row on linux");
  // The default set is unscoped: it keeps its cross rows (the one foreign OS its platforms scope names); the scoped sets plan none, so no row names metal or vulkan for a foreign target.
  assert.deepEqual(rowIds(darwin).filter((id) => id.includes("@")), ["clippy:fx:0@x86_64-pc-windows-gnu"]);
  assert.deepEqual(rowIds(win).filter((id) => id.includes("@")), ["clippy:fx:0@aarch64-apple-darwin"]);
  assert.ok(!rowIds(darwin).some((id) => /^clippy:fx:[12]@/.test(id)));
  // No default `--all-features` check row and no `regen` feature on any row: it is unlinted.
  for (const result of [darwin, win]) {
    assert.ok(!rowIds(result).some((id) => id.startsWith("check-features")), "checkFeatureSets [[]] adds no check row");
    for (const id of rowIds(result)) assert.ok(!/regen|all-features/.test(JSON.stringify(result.scans.find((s) => s.id === id).args)), id);
  }
});

test("planning: every set scoped to other hosts means no compile row and a note", (t) => {
  const root = fixture();
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const only = { featureSets: [{ features: [], hosts: ["darwin"], hostsReason: "fixture" }] };
  const darwin = plan(root, "darwin", { fx: only });
  const win = plan(root, "win32", { fx: only });
  assert.deepEqual(rowIds(darwin).filter((id) => !id.includes("@")), ["check-features:fx:0", "clippy:fx", "doc:fx", "test:rust:fx"], "checkFeatureSets is absent: the default --all-features check, as today");
  assert.deepEqual(rowIds(darwin).filter((id) => id.includes("@")), [], "a native-host set has no cross row");
  assert.deepEqual(rowIds(win), []);
  assert.ok(win.notes.some((n) => /fx: every declared feature set is scoped to other hosts/.test(n)));
});

test("planning: a crate with only array sets plans the same rows on every host as before (cross rows included)", (t) => {
  const root = fixture();
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const plain = { featureSets: [[], ["--features", "metal"]], checkFeatureSets: [["--features", "vulkan"]] };
  const darwin = plan(root, "darwin", { fx: plain });
  assert.deepEqual(rowIds(darwin), [
    "check-features:fx:0", "clippy:fx:0", "clippy:fx:1", "clippy:fx:0@x86_64-unknown-linux-gnu", "clippy:fx:1@x86_64-unknown-linux-gnu",
    "clippy:fx:0@x86_64-pc-windows-gnu", "clippy:fx:1@x86_64-pc-windows-gnu", "doc:fx", "test:rust:fx:0", "test:rust:fx:1",
  ].sort((a, b) => rowIds(darwin).indexOf(a) - rowIds(darwin).indexOf(b)));
});

test("no-change proof: rustCratePlan equals the pre-scoping formula for every existing array-only crate entry on every host", () => {
  const config = JSON.parse(fs.readFileSync(path.join(repoRoot, "ci", "gate.config.json"), "utf8")).rust;
  const legacy = (own, auto) => {
    const declared = Array.isArray(own?.featureSets) && own.featureSets.length ? own.featureSets : [[]];
    const lintSets = auto ? [[], ["--all-features"]] : declared;
    const defaultCheck = !lintSets.some((set) => set.length === 0);
    const checkSets = (lintSets.some((set) => set.includes("--all-features")) ? [] : (Array.isArray(own?.checkFeatureSets) ? own.checkFeatureSets : [["--all-features"]])).filter((set) => set.length);
    return { lintSets, baseSets: declared, defaultCheck, checkSets };
  };
  let compared = 0;
  for (const [name, own] of Object.entries(config)) {
    if (hasHostScopedSets(own, name)) continue;
    for (const auto of [false, true]) for (const platform of HOSTS) {
      const entry = auto ? { ...own, autoAllFeatures: true } : own;
      const got = rustCratePlan(entry, platform, name);
      const want = legacy(entry, auto && !(Array.isArray(own.featureSets) && own.featureSets.length));
      assert.deepEqual({ lintSets: got.lintSets, baseSets: got.baseSets, defaultCheck: got.defaultCheck, checkSets: got.checkSets }, want, `${name} ${platform}`);
      assert.equal(got.crossSets, got.baseSets, `${name}: cross rows keep every declared set`);
      assert.equal(got.none, false);
      compared += 1;
    }
  }
  assert.ok(compared > 100, `compared ${compared} crate plans`);
  // The only host-scoped entries in the repository: the three tts crates (Metal/Vulkan/BLAS) and the three with a sqlcipher-system check set.
  assert.deepEqual(Object.entries(config).filter(([name, own]) => hasHostScopedSets(own, name)).map(([name]) => name).sort(), ["rightkit-bench", "rightkit-credentials", "rightkit-db", "rightkit-tts", "rightkit-tts-qwen", "rightkit-tts-qwen-sys"]);
});

test("no-change proof: planScans rows of existing crates come from the configured array sets exactly", () => {
  const config = JSON.parse(fs.readFileSync(path.join(repoRoot, "ci", "gate.config.json"), "utf8")).rust;
  const names = ["rightkit-db", "rightkit-onnx", "rightkit-mcp", "rightkit-child-process"].filter((n) => config[n] && fs.existsSync(path.join(repoRoot, "crates", n, "Cargo.toml")));
  assert.ok(names.length >= 2);
  for (const platform of ["darwin", "win32"]) {
    const result = plan(repoRoot, platform, config, names);
    for (const name of names) {
      const sets = Array.isArray(config[name].featureSets) && config[name].featureSets.length ? config[name].featureSets : [[]];
      const ids = new Set(result.scans.map((s) => s.id));
      sets.forEach((_set, i) => assert.ok(ids.has(sets.length === 1 ? `test:rust:${name}` : `test:rust:${name}:${i}`), `${name} ${platform} test ${i}`));
      for (const target of Object.values(CROSS_TARGETS).filter((t) => t !== CROSS_TARGETS[platform])) {
        sets.forEach((_set, i) => assert.ok(ids.has(`${sets.length === 1 ? `clippy:${name}` : `clippy:${name}:${i}`}@${target}`), `${name} ${platform} cross ${target} ${i}`));
      }
    }
  }
});

test("requiredScanIds: a host's required set is its own sets; a crate with host-scoped sets needs the platform", () => {
  const exists = (p) => p === "crates/fx/src/lib.rs";
  const ids = (platform) => requiredScanIds({ units: ["fx"], crateFeatures: { fx: SCOPED }, platform, exists });
  assert.ok(ids("darwin").includes("clippy:fx:1") && !ids("darwin").includes("clippy:fx:2"));
  assert.ok(ids("win32").includes("clippy:fx:1") && ids("darwin").includes("doc:fx"));
  assert.throws(() => requiredScanIds({ units: ["fx"], crateFeatures: { fx: SCOPED }, exists }), /host-scoped sets: the host's platform/);
  // an array-only crate needs no platform
  assert.ok(requiredScanIds({ units: ["fx"], crateFeatures: { fx: { featureSets: [[], ["--features", "a"]] } }, exists }).includes("clippy:fx:1"));
});

// ---- coverage ------------------------------------------------------------------------------------------------------------------

test("coverage: a feature is covered when any declared set enables it, whatever its hosts", () => {
  const sets = rustCoverageSets(SCOPED, "fx");
  assert.deepEqual(uncoveredFeatures(MANIFEST, sets, SCOPED.unlintedFeatures), ["native"], "native is in no set; metal and vulkan are covered by the darwin and win32 sets, regen is unlinted");
  const covered = { ...SCOPED, featureSets: [...SCOPED.featureSets, { features: ["--features", "native"], hosts: ["linux"], hostsReason: "fixture" }] };
  assert.deepEqual(featureCoverageProblems({ name: "fx", manifestText: MANIFEST, sets: rustCoverageSets(covered, "fx"), unlinted: covered.unlintedFeatures }), []);
  const problems = featureCoverageProblems({ name: "fx", manifestText: MANIFEST, sets, unlinted: SCOPED.unlintedFeatures });
  assert.equal(problems.length, 1);
  assert.match(problems[0], /^FEATURE_UNCOVERED fx: feature "native"/);
});

test("coverage: a feature named in unlintedFeatures counts as covered; an unknown name or a name a set enables is an error", () => {
  const sets = [[], ["--features", "metal"], ["--features", "vulkan"], ["--features", "native"]];
  assert.deepEqual(featureCoverageProblems({ name: "fx", manifestText: MANIFEST, sets, unlinted: { regen: "why" } }), []);
  assert.match(featureCoverageProblems({ name: "fx", manifestText: MANIFEST, sets }).join("\n"), /FEATURE_UNCOVERED fx: feature "regen"/);
  const unknown = featureCoverageProblems({ name: "fx", manifestText: MANIFEST, sets, unlinted: { regen: "why", nosuch: "why" } });
  assert.equal(unknown.length, 1);
  assert.match(unknown[0], /UNLINTED_FEATURE_UNKNOWN fx: unlintedFeatures names "nosuch"/);
  const enabled = featureCoverageProblems({ name: "fx", manifestText: MANIFEST, sets: [...sets, ["--features", "regen"]], unlinted: { regen: "why" } });
  assert.equal(enabled.length, 1);
  assert.match(enabled[0], /UNLINTED_FEATURE_ENABLED fx: unlintedFeatures names "regen" but a declared feature set enables it/);
  const viaAll = featureCoverageProblems({ name: "fx", manifestText: MANIFEST, sets: [["--all-features"]], unlinted: { regen: "why" } });
  assert.match(viaAll.join("\n"), /UNLINTED_FEATURE_ENABLED fx/);
  assert.match(featureCoverageProblems({ name: "fx", manifestText: MANIFEST, sets, unlinted: { default: "why" } }).join("\n"), /UNLINTED_FEATURE_UNKNOWN/);
});

test("coverage: the feature-coverage scan plans a crate that has unlintedFeatures, and it carries them", (t) => {
  const root = fixture();
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const result = plan(root, "linux", { fx: { ...SCOPED, platforms: undefined, platformsReason: undefined } });
  const scan = result.scans.find((s) => s.id === "feature-coverage");
  assert.deepEqual(scan.crates.map((c) => c.name), ["fx"]);
  assert.deepEqual(scan.crates[0].unlinted, { regen: "rewrites checked-in files" });
  assert.deepEqual(scan.crates[0].sets, rustCoverageSets({ ...SCOPED, platforms: undefined }, "fx"));
});

test("a crate with no explicit sets and unlintedFeatures gets a generated set that never enables them (no --all-features)", () => {
  const [name, own] = ["fx", { unlintedFeatures: { regen: "why" } }];
  const effective = effectiveCrateFeatures({ crateFeatures: { [name]: own }, units: [name], manifestOf: () => MANIFEST })[name];
  assert.deepEqual(effective.autoFeatures, ["metal", "native", "vulkan"]);
  const planned = rustCratePlan(effective, "darwin", name);
  assert.deepEqual(planned.lintSets, [[], ["--features", "metal,native,vulkan"]]);
  assert.deepEqual(planned.checkSets, []);
  assert.deepEqual(featureCoverageProblems({ name, manifestText: MANIFEST, sets: rustCoverageSets(effective, name), unlinted: own.unlintedFeatures }), []);
  // without unlintedFeatures the generated set is --all-features, as before
  const plain = effectiveCrateFeatures({ crateFeatures: { [name]: {} }, units: [name], manifestOf: () => MANIFEST })[name];
  assert.deepEqual(rustCratePlan(plain, "darwin", name).lintSets, [[], ["--all-features"]]);
  assert.throws(() => effectiveCrateFeatures({ crateFeatures: { [name]: { unlintedFeatures: { regen: "" } } }, units: [name], manifestOf: () => MANIFEST }), /rust\.fx\.unlintedFeatures\.regen needs a non-empty reason/);
  assert.throws(() => effectiveCrateFeatures({ crateFeatures: { [name]: { featureSets: [{ features: [], hosts: ["mac"], hostsReason: "x" }] } }, units: [name], manifestOf: () => MANIFEST }), /rust\.fx\.featureSets\[0\]\.hosts/);
});

// ---- the repository's own entries ------------------------------------------------------------------------------------------------

test("ci/gate.config.json: the four host-scoped crates are linted on the host that can build them, nothing is uncovered", () => {
  const config = JSON.parse(fs.readFileSync(path.join(repoRoot, "ci", "gate.config.json"), "utf8")).rust;
  const sets = (name, platform) => hostFeatureSets(config[name], platform, "featureSets", name).map((set) => set.join(" ") || "default");
  assert.deepEqual(sets("rightkit-tts-qwen-sys", "darwin"), ["default", "--features native", "--features metal,blas"]);
  assert.deepEqual(sets("rightkit-tts-qwen-sys", "win32"), ["default", "--features native", "--features vulkan"]);
  assert.deepEqual(sets("rightkit-tts-qwen-sys", "linux"), []);
  for (const platform of HOSTS) assert.deepEqual(crossFeatureSets(config["rightkit-tts-qwen-sys"], platform), [], "qwen-sys has no foreign-OS lint row on any host");
  assert.deepEqual(unlintedFeatures(config["rightkit-tts-qwen-sys"], "rightkit-tts-qwen-sys") && Object.keys(unlintedFeatures(config["rightkit-tts-qwen-sys"])), ["regenerate-bindings"]);
  const credentialsCheck = (platform) => hostFeatureSets(config["rightkit-credentials"], platform, "checkFeatureSets", "rightkit-credentials").map((set) => set.join(" "));
  assert.equal(credentialsCheck("win32").length, 1);
  assert.equal(credentialsCheck("darwin").length, 2);
  assert.ok(credentialsCheck("linux").some((s) => s.includes("sqlcipher-system")) && !credentialsCheck("win32").some((s) => s.includes("sqlcipher-system")));
  for (const name of ["rightkit-tts-qwen-sys", "rightkit-tts-qwen", "rightkit-tts", "rightkit-credentials"]) {
    const manifest = fs.readFileSync(path.join(repoRoot, "crates", name, "Cargo.toml"), "utf8");
    const own = config[name];
    assert.deepEqual(featureCoverageProblems({ name, manifestText: manifest, sets: rustCoverageSets(own, name), unlinted: unlintedFeatures(own, name) }), [], name);
    for (const platform of HOSTS) rustCratePlan(own, platform, name);
  }
});

// ---- local publication and the evidence it must show --------------------------------------------------------------------------------

test("localTestPlan: a host plans its own sets; a host-scoped set has no cross-target step and no refusal for an unprovisioned zig", () => {
  const scopedOnly = { rust: { fx: { platforms: ["darwin", "win32"], platformsReason: "fixture", featureSets: [{ features: [], hosts: ["darwin", "win32"], hostsReason: "x" }, { features: ["--features", "metal"], hosts: ["darwin"], hostsReason: "x" }], checkFeatureSets: [[]] } } };
  const stepsOf = (config, platform) => localTestPlan({ kind: "cargo", unit: "fx", dir: "crates/fx", config, batch: false, platform }).steps.map((step) => step.args.join(" "));
  const saved = process.env.RIGHTKIT_CROSS_CC_DIR;
  delete process.env.RIGHTKIT_CROSS_CC_DIR;
  try {
    const darwin = stepsOf(scopedOnly, "darwin");
    assert.ok(darwin.some((s) => s.startsWith("clippy") && s.includes("--features metal")));
    assert.ok(!darwin.some((s) => s.includes("--target")), "no cross step for a native-host set");
    assert.ok(!stepsOf(scopedOnly, "win32").some((s) => s.includes("metal")));
    assert.equal(localTestPlan({ kind: "cargo", unit: "fx", dir: "crates/fx", config: scopedOnly, platform: "win32" }).featureLabels.join("|"), "default");
    assert.deepEqual(localTestPlan({ kind: "cargo", unit: "fx", dir: "crates/fx", config: scopedOnly, platform: "linux" }).featureLabels, ["not built on linux (ci/gate.config.json platforms)"]);
    // an unscoped set keeps its cross steps
    const mixed = { rust: { fx: { platforms: ["darwin", "win32"], platformsReason: "fixture", featureSets: [[], { features: ["--features", "metal"], hosts: ["darwin"], hostsReason: "x" }] } } };
    process.env.RIGHTKIT_CROSS_CC_DIR = path.join(scratch, "zig-fixture");
    const cross = stepsOf(mixed, "darwin").filter((s) => s.includes("--target"));
    assert.equal(cross.length, 1);
    assert.ok(!cross[0].includes("metal"));
  } finally { if (saved === undefined) delete process.env.RIGHTKIT_CROSS_CC_DIR; else process.env.RIGHTKIT_CROSS_CC_DIR = saved; }
});

test("batchClippyInvocations: only the sets crossSetsOf names are linted for foreign targets", () => {
  const saved = process.env.RIGHTKIT_CROSS_CC_DIR;
  process.env.RIGHTKIT_CROSS_CC_DIR = path.join(scratch, "zig-fixture");
  try {
    const sets = [[], ["--features", "metal"]];
    const all = batchClippyInvocations(["fx"], { featureSetsOf: () => sets, crossTargetsOf: () => ["t1"] });
    assert.equal(all.length, 4, "default behaviour: every set, native and cross");
    const scoped = batchClippyInvocations(["fx"], { featureSetsOf: () => sets, crossSetsOf: () => [[]], crossTargetsOf: () => ["t1"] });
    assert.equal(scoped.length, 3);
    assert.equal(scoped.filter((i) => i.args.includes("--target")).length, 1);
    assert.ok(!scoped.some((i) => i.args.includes("--target") && i.args.includes("metal")));
  } finally { if (saved === undefined) delete process.env.RIGHTKIT_CROSS_CC_DIR; else process.env.RIGHTKIT_CROSS_CC_DIR = saved; }
});

test("publication evidence: the batch must show the sets of the host that produced it, and no cross row for a host-scoped set", () => {
  const commit = "a".repeat(40); const tree = "b".repeat(40); const workspace = "crates/fx";
  const evidence = { name: "fx", commit };
  const row = (features, target) => ({
    name: "clippy", ok: true, commit, tree, workspace, crates: ["fx"],
    command: ["cargo", "clippy", "-p", "fx", "--message-format=json-render-diagnostics", "--no-deps", "--all-targets", ...(target ? ["--target", target] : []), ...features, "--", "-D", "warnings"].join(" "),
  });
  const batchOf = (...rows) => ({ tree, workspaces: { fx: workspace }, checks: rows });
  const missing = (batch, platform) => batchClippyMissingForHost(batch, evidence, SCOPED, platform, workspace).map(({ features, target }) => `${features.join(" ") || "default"} @ ${target ?? "native"}`);
  // darwin produced: default (native + the one foreign OS its platforms scope names, windows) and metal (native only)
  const darwinBatch = batchOf(row([]), row([], CROSS_TARGETS.win32), row(["--features", "metal"]));
  assert.deepEqual(missing(darwinBatch, "darwin"), []);
  assert.deepEqual(missing(batchOf(row([]), row(["--features", "metal"])), "darwin"), [`default @ ${CROSS_TARGETS.win32}`]);
  // the same batch from win32 is not evidence of win32's sets: vulkan is missing
  assert.deepEqual(missing(darwinBatch, "win32").sort(), [`default @ ${CROSS_TARGETS.darwin}`, "--features vulkan @ native"].sort());
  // a metal row for a foreign target is never required
  assert.ok(!missing(batchOf(row([])), "darwin").some((m) => m.startsWith("metal") && !m.endsWith("@ native")));
  // every declared set scoped to other hosts: nothing this host's batch could prove
  const other = { featureSets: [{ features: [], hosts: ["win32"], hostsReason: "x" }] };
  assert.equal(batchClippyMissingForHost(darwinBatch, evidence, other, "darwin", workspace).length, 1);
  // an entry with no object keeps today's requirement: every set, native and every foreign target of its scope
  assert.deepEqual(batchClippyMissingForHost(batchOf(row([])), evidence, { featureSets: [[]] }, "darwin", workspace).map((m) => m.target), [CROSS_TARGETS.linux, CROSS_TARGETS.win32]);
});
