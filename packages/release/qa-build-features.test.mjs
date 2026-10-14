import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import test from "node:test";
import {
  assertNoForbiddenBuildFeatures, buildFeatureIssues, cargoManifestIssues, commandFeatureIssues, isForbiddenFeature, resolveCargoManifestPath, tokenize,
} from "./qa-build-features.mjs";

const issues = (command, scripts = {}) => commandFeatureIssues(command, { label: "target.prePackage", scripts });

test("forbidden feature names, including dep:, crate/ and crate?/ forms", () => {
  for (const f of ["devtools", "qa-native", "qa-control", "dev-native", "dev-inspect", "qa-shot", "qa-tools", "dev-control", "bench-native", "heardright/qa-shot", "dep:qa-tools", "rightkit-tauri-shell/dev-inspect", "rightkit-tauri-shell?/dev-inspect", "tauri/devtools", "tauri?/devtools", "rightkit-qa-driver/qa-control", "dep:tauri-plugin-wdio-webdriver"]) assert.equal(isForbiddenFeature(f), true, f);
  for (const f of ["custom-protocol", "tauri-shell", "dep:rightkit-qa-driver", "wv2-0_39", "tauri/custom-protocol"]) assert.equal(isForbiddenFeature(f), false, f);
});

test("a build command that enables a forbidden feature is refused in every spelling", () => {
  const cases = [
    { cmd: "pnpm", args: ["tauri", "build", "--features", "qa-native"] },
    { cmd: "cargo", args: ["build", "--release", "--features=custom-protocol,devtools"] },
    { cmd: "cargo", args: ["build", "-F", "qa-control"] },
    { cmd: "cargo", args: ["build", "-Fdev-native"] },
    { cmd: "cargo", args: ["build", "--release", "--features", "rightkit-tauri-shell/dev-inspect"] },
    { cmd: "cargo", args: ["build", "--features=custom-protocol,dev-inspect"] },
    { cmd: "cargo", args: ["build"], env: { CARGO_FEATURES: "rightkit-tauri-shell/dev-inspect" } },
    { cmd: "pnpm", args: ["tauri", "build", "-f", "tauri/devtools"] },
    { cmd: "pnpm", args: ["tauri", "build", "--", "--features", "a b qa-native"] },
    { cmd: "sh", args: ["-c", "cargo build --release --features 'custom-protocol qa-native'"] },
    { cmd: "cargo", args: ["build", "--all-features"] },
    { cmd: "cargo", args: ["build"], env: { CARGO_FEATURES: "devtools" } },
    "cargo build --features qa-native",
  ];
  for (const command of cases) assert.notDeepEqual(issues(command), [], JSON.stringify(command));
});

test("clean build commands pass, including similar words", () => {
  for (const command of [
    { cmd: "pnpm", args: ["tauri", "build", "--features", "custom-protocol"] },
    { cmd: "cargo", args: ["build", "--release", "--no-default-features"] },
    { cmd: "node", args: ["scripts/devtools-docs.mjs"] },
    undefined,
  ]) assert.deepEqual(command === undefined ? [] : issues(command), []);
});

test("a package script that the build runs is followed up to three levels", () => {
  const scripts = { "build:win": "pnpm run tauri:build", "tauri:build": "tauri build --features qa-native", ok: "tauri build" };
  const found = issues({ cmd: "pnpm", args: ["run", "build:win"] }, scripts);
  assert.equal(found.length, 1);
  assert.match(found[0], /build:win.*tauri:build.*qa-native/);
  assert.deepEqual(issues({ cmd: "pnpm", args: ["run", "ok"] }, scripts), []);
  assert.notDeepEqual(issues({ cmd: "pnpm", args: ["build:win"] }, scripts), []);
});

const manifest = (features, dependencies) => `[package]\nname = "app"\n[features]\n${features}\n[dependencies]\n${dependencies}\n`;

test("Cargo.toml: devtools on tauri, qa-control on rightkit-qa-driver, non-optional wdio plugin, forbidden default closure", () => {
  assert.match(cargoManifestIssues(manifest('default = []', 'tauri = { version = "2.11.5", features = ["devtools"] }'))[0], /devtools/);
  assert.match(cargoManifestIssues(manifest('default = []', 'rightkit-qa-driver = { version = "0.3", features = ["qa-control", "wv2-0_39"] }'))[0], /qa-control/);
  assert.match(cargoManifestIssues(manifest('default = []', 'rightkit-control = { version = "0.3", features = ["qa-control", "wv2-0_39"] }'))[0], /rightkit-control.*qa-control/);
  assert.match(cargoManifestIssues(manifest('default = []', 'tauri-plugin-wdio-webdriver = "1.4.0"'))[0], /not optional/);
  assert.match(cargoManifestIssues(manifest('default = ["shell"]\nshell = ["qa-native"]\nqa-native = ["dep:tauri-plugin-wdio-webdriver"]', 'tauri-plugin-wdio-webdriver = { version = "1.4.0", optional = true }'))[0], /default feature set reaches `qa-native`/);
  assert.match(cargoManifestIssues(manifest('default = ["tauri/devtools"]', 'tauri = "2"')).join("\n"), /reaches `tauri\/devtools`/);
  const table = `[features]\ndefault = []\n[dependencies.tauri]\nversion = "2"\nfeatures = ["devtools"]\n`;
  assert.match(cargoManifestIssues(table)[0], /devtools/);
});

test("Cargo.toml: dev-inspect on rightkit-tauri-shell, directly, through default, or through another feature", () => {
  assert.match(cargoManifestIssues(manifest("default = []", 'rightkit-tauri-shell = { version = "0.4", features = ["dev-inspect"] }'))[0], /dev-inspect/);
  assert.match(cargoManifestIssues(manifest('default = ["rightkit-tauri-shell/dev-inspect"]', 'rightkit-tauri-shell = "0.4"')).join("\n"), /reaches `rightkit-tauri-shell\/dev-inspect`/);
  assert.match(cargoManifestIssues(manifest('default = ["shell"]\nshell = ["dev-native"]\ndev-native = ["rightkit-tauri-shell/dev-inspect"]', 'rightkit-tauri-shell = "0.4"')).join("\n"), /reaches `dev-native`/);
  assert.match(cargoManifestIssues(manifest('default = ["x"]\nx = ["rightkit-tauri-shell?/dev-inspect"]', 'rightkit-tauri-shell = { version = "0.4", optional = true }')).join("\n"), /dev-inspect/);
  // the allow-list for ContentOnly devtools never lifts dev-inspect
  assert.match(cargoManifestIssues(manifest('default = ["rightkit-tauri-shell/dev-inspect"]', 'rightkit-tauri-shell = "0.4"'), "Cargo.toml", { allowDevtools: true }).join("\n"), /dev-inspect/);
  assert.equal(isForbiddenFeature("dev-inspect", { allowDevtools: true }), true);
});

test("Cargo.toml: an optional dev-native = [rightkit-tauri-shell/dev-inspect] feature that default does not reach is accepted", () => {
  const ok = manifest('default = ["tauri-shell"]\ntauri-shell = []\ndev-native = ["rightkit-tauri-shell/dev-inspect"]', 'rightkit-tauri-shell = { version = "0.4" }');
  assert.deepEqual(cargoManifestIssues(ok), []);
});

test("Cargo.toml: dev-inspect on rightkit-tauri-shell or the old rightkit-shell name is refused unless optional", () => {
  for (const name of ["rightkit-tauri-shell", "rightkit-shell"]) {
    assert.match(cargoManifestIssues(manifest('default = []', `${name} = { version = "0.4", features = ["dev-inspect"] }`))[0], new RegExp(`${name}.*dev-inspect`));
    assert.deepEqual(cargoManifestIssues(manifest('default = []', `${name} = { version = "0.4", optional = true, features = ["dev-inspect"] }`)), []);
  }
});

test("Cargo.toml: the optional, off-by-default qa-native layout used by the apps is accepted", () => {
  const ok = manifest('default = ["tauri-shell"]\nqa-native = ["tauri-shell", "dep:tauri-plugin-wdio-webdriver", "tauri/custom-protocol"]\ntauri-shell = []',
    'tauri = { version = "2.11.5", features = ["custom-protocol"] }\n# tauri = { features = ["devtools"] }\ntauri-plugin-wdio-webdriver = { version = "1.2.0", optional = true }\nrightkit-qa-driver = { version = "0.3", optional = true }\nrightkit-control = { version = "0.3", optional = true }');
  assert.deepEqual(cargoManifestIssues(ok), []);
});

test("buildFeatureIssues reads package.json scripts and src-tauri/Cargo.toml; the assert throws with every finding", () => {
  const root = mkdtempSync(join(tmpdir(), "rk-qa-features-"));
  mkdirSync(join(root, "src-tauri"));
  writeFileSync(join(root, "package.json"), JSON.stringify({ scripts: { "bundle": "tauri build --features qa-native" } }));
  writeFileSync(join(root, "src-tauri", "Cargo.toml"), manifest("default = []", 'tauri = { version = "2", features = ["devtools"] }'));
  const commands = [{ label: "target.prePackage", command: { cmd: "pnpm", args: ["run", "bundle"] } }, { label: "target.package", command: undefined }];
  assert.equal(buildFeatureIssues({ root, commands }).length, 2);
  assert.throws(() => assertNoForbiddenBuildFeatures({ root, commands, tier: "update" }), /update build refused[\s\S]*qa-native[\s\S]*devtools/);
  const clean = mkdtempSync(join(tmpdir(), "rk-qa-features-"));
  assert.doesNotThrow(() => assertNoForbiddenBuildFeatures({ root: clean, commands: [{ label: "x", command: { cmd: "pnpm", args: ["tauri", "build"] } }] }));
});

test("tokenize keeps quoted groups together", () => {
  assert.deepEqual(tokenize("a --features 'b c' \"d\""), ["a", "--features", "b c", "d"]);
});

test("M20: qa-shot, qa-tools, dev-control and bench-native are refused in build commands and default closures", () => {
  for (const f of ["qa-shot", "qa-tools", "dev-control", "bench-native"]) {
    assert.ok(issues(`cargo tauri build --features ${f}`).length > 0, f);
    assert.ok(issues(`cargo build -F ${f}`).length > 0, f);
  }
  for (const f of ["qa-shot", "qa-tools", "dev-control", "bench-native"]) {
    assert.match(cargoManifestIssues(manifest(`default = ["${f}"]\n${f} = []`, 'tauri = "2"')).join("\n"), new RegExp(`reaches \`${f}\``), f);
  }
  assert.deepEqual(cargoManifestIssues(manifest('default = []\nqa-shot = []\nbench-native = []', 'tauri = "2"')), []);
});

test("buildFeatureIssues checks a declared cargoManifest in a non-standard directory; default test-mode there is reported", () => {
  const root = mkdtempSync(join(tmpdir(), "rk-qa-features-"));
  mkdirSync(join(root, "engine", "app"), { recursive: true });
  writeFileSync(join(root, "engine", "app", "Cargo.toml"), manifest('default = ["test-mode"]\ntest-mode = []', 'tauri = "2"'));
  const found = buildFeatureIssues({ root, commands: [], cargoManifest: "engine/app/Cargo.toml" });
  assert.deepEqual(found, ["engine/app/Cargo.toml: default feature set reaches `test-mode`"]);
  assert.throws(() => assertNoForbiddenBuildFeatures({ root, commands: [], cargoManifest: "engine/app/Cargo.toml", tier: "release" }), /release build refused[\s\S]*test-mode/);
  assert.deepEqual(buildFeatureIssues({ root, commands: [], manifest: "engine/app/Cargo.toml" }), found);
});

test("buildFeatureIssues reads the declared manifest, not src-tauri/Cargo.toml, when both exist", () => {
  const root = mkdtempSync(join(tmpdir(), "rk-qa-features-"));
  mkdirSync(join(root, "src-tauri"));
  mkdirSync(join(root, "engine"));
  writeFileSync(join(root, "src-tauri", "Cargo.toml"), manifest('default = ["test-mode"]\ntest-mode = []', 'tauri = "2"'));
  writeFileSync(join(root, "engine", "Cargo.toml"), manifest('default = []', 'tauri = "2"'));
  assert.deepEqual(buildFeatureIssues({ root, commands: [], cargoManifest: "engine/Cargo.toml" }), []);
  assert.match(buildFeatureIssues({ root, commands: [] }).join("\n"), /src-tauri[\\/]Cargo\.toml: default feature set reaches `test-mode`/);
});

test("resolveCargoManifestPath order: cargoManifest, releaseSwitches.manifest, src-tauri/Cargo.toml, Cargo.toml", () => {
  const rootWith = (...rels) => {
    const root = mkdtempSync(join(tmpdir(), "rk-qa-resolve-"));
    for (const rel of rels) {
      mkdirSync(join(root, dirname(rel)), { recursive: true });
      writeFileSync(join(root, rel), '[package]\nname = "x"\n');
    }
    return root;
  };
  const all = ["Cargo.toml", join("src-tauri", "Cargo.toml"), join("rs", "Cargo.toml"), join("engine", "Cargo.toml")];
  assert.equal(resolveCargoManifestPath({ root: rootWith(...all), cargoManifest: "engine/Cargo.toml", manifest: "rs/Cargo.toml" }).rel, "engine/Cargo.toml");
  assert.equal(resolveCargoManifestPath({ root: rootWith(...all), manifest: "rs/Cargo.toml" }).rel, "rs/Cargo.toml");
  assert.equal(resolveCargoManifestPath({ root: rootWith(...all) }).rel, join("src-tauri", "Cargo.toml"));
  assert.equal(resolveCargoManifestPath({ root: rootWith("Cargo.toml") }).rel, "Cargo.toml");
  // A declared path that is missing falls through to the next existing candidate; the closure gate separately refuses a missing declared manifest.
  const fallthrough = resolveCargoManifestPath({ root: rootWith(join("src-tauri", "Cargo.toml")), cargoManifest: "gone/Cargo.toml" });
  assert.equal(fallthrough.rel, join("src-tauri", "Cargo.toml"));
  assert.deepEqual(fallthrough.declared, ["gone/Cargo.toml"]);
  const none = resolveCargoManifestPath({ root: rootWith(), cargoManifest: "gone/Cargo.toml" });
  assert.equal(none.rel, undefined);
  assert.deepEqual(none.candidates, ["gone/Cargo.toml", join("src-tauri", "Cargo.toml"), "Cargo.toml"]);
});
