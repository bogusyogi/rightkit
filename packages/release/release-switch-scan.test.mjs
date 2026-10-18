import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { cargoManifestIssues, commandFeatureIssues } from "./qa-build-features.mjs";
import {
  REGISTRY, TEST_MODE_MARKER, assertNoReleaseSwitches, bufferSwitchFindings, commandSwitchFindings, denyNamesFor,
  featureClosureFindings, inventoryDenyNames, parseInventory, remoteServiceFindings, resolveFeatureClosure, releaseSwitchFindings, scanSwitchPaths, sourceSwitchFindings,
} from "./release-switch-scan.mjs";

const FIX = new URL("./fixtures/release-switches/", import.meta.url);
const fixture = (rel) => readFileSync(new URL(rel, FIX), "utf8");
const MACHO = Buffer.from([0xcf, 0xfa, 0xed, 0xfe]);
const PE = Buffer.from("MZ");
const filler = (n) => Buffer.alloc(n, 0x90);
const tmp = () => mkdtempSync(join(tmpdir(), "rk-switch-scan-"));
const ids = (findings) => findings.map((f) => f.rule);

test("the marker matches the Rust constant and the registry", () => {
  assert.equal(TEST_MODE_MARKER, "RIGHTKIT-TEST-MODE-COMPILED");
  assert.equal(REGISTRY.testModeMarker, TEST_MODE_MARKER);
  const rust = readFileSync(new URL("../../crates/rightkit-daemon/src/mode.rs", import.meta.url), "utf8");
  assert.ok(rust.includes(`"${TEST_MODE_MARKER}"`), "mode.rs TEST_MODE_MARKER_TEXT must match");
});

test("the registry lists the spec's initial deny names with valid classes", () => {
  const names = new Set(REGISTRY.denyNames.map((d) => d.name));
  for (const required of ["RIGHTKIT_QA_NATIVE", "RIGHTKIT_UNDER_TEST", "RIGHTKIT_SUITE_ROOT", "RIGHT_SUITE_DIR", "ORTHIC_SHARED_DIR", "HEARDRIGHT_ENGINE_TEST_MODE", "MAILRIGHT_DATA_DIR", "GENRIGHT_QA", "CODERIGHT_HOME", "POSTRIGHT_ROLE=daemon-dev", "LIGHTCRAFT_SAM3_DIR", "--qa", "--demo"]) {
    assert.ok(names.has(required), required);
  }
  for (const d of REGISTRY.denyNames) assert.match(d.class, /^S[1-4]$/, d.name);
});

// N6: source layer refuses a build with the feature on; binary layer refuses independently.
test("N6 source layer: a release build with test-mode on is refused in every form", () => {
  const closure = featureClosureFindings(fixture("tree-with-test-mode.txt"));
  assert.ok(closure.length >= 1);
  assert.ok(ids(closure).every((id) => id === "forbidden-feature test-mode"));
  assert.ok(closure.some((f) => /rightkit-daemon/.test(f.reason)));

  for (const argv of [["cargo", "build", "--release", "--features", "test-mode"], ["cargo", "build", "-F", "rightkit-daemon/test-mode"], ["cargo", "build", "--features=custom-protocol,test-mode"], ["cargo", "build", "-Ftest-mode"]]) {
    assert.deepEqual(ids(commandSwitchFindings(argv)), ["forbidden-feature test-mode"], argv.join(" "));
  }
  assert.deepEqual(ids(commandSwitchFindings(["cargo", "build", "--all-features"])), ["forbidden-feature all-features"]);

  // The existing source-layer scanners agree (right-release already calls these).
  assert.ok(commandFeatureIssues({ cmd: "cargo", args: ["build", "--features", "test-mode"] }, { label: "prePackage" }).length > 0);
  const manifest = fixture("app/Cargo.toml");
  assert.deepEqual(cargoManifestIssues(manifest, "fixture"), []);
  const enabledByDefault = manifest.replace('default = []', 'default = ["test-mode"]');
  assert.ok(cargoManifestIssues(enabledByDefault, "fixture").some((i) => /test-mode/.test(i)));
  const dep = `${manifest}\n[dependencies]\nrightkit-daemon = { version = "0.1", features = ["test-mode"] }\n`;
  assert.ok(cargoManifestIssues(dep, "fixture").some((i) => /rightkit-daemon.*test-mode/.test(i)));
});

test("N6 binary layer: with the source check bypassed, a binary carrying the marker is refused; the clean twin passes", () => {
  const root = tmp();
  const withFeature = join(root, "app-test-mode");
  const clean = join(root, "app-clean");
  writeFileSync(withFeature, Buffer.concat([MACHO, filler(300), Buffer.from(TEST_MODE_MARKER), filler(300), Buffer.from("FIXTURE_QA_ROOT")]));
  writeFileSync(clean, Buffer.concat([MACHO, filler(600), Buffer.from("fixture")]));

  const bad = releaseSwitchFindings({ binaryPaths: [withFeature], bypass: ["feature-closure"] });
  assert.ok(ids(bad.findings).includes("test-mode-capability"));
  assert.throws(() => assertNoReleaseSwitches({ binaryPaths: [withFeature], bypass: ["feature-closure"] }, "release"), /test-mode-capability/);

  const good = releaseSwitchFindings({ binaryPaths: [clean], treeText: fixture("tree-clean.txt"), sourceRoot: new URL("app/", FIX).pathname, inventoryText: fixture("app/switches.json") });
  assert.deepEqual(good.findings, []);
  assert.equal(good.receipt.binaries, 1);
  assert.match(good.receipt.files[0].sha256, /^[0-9a-f]{64}$/);
  // None of the deny names or the marker is in the clean binary.
  const bytes = readFileSync(clean);
  for (const d of REGISTRY.denyNames) assert.equal(bytes.includes(Buffer.from(d.name)), false, d.name);
  assert.equal(bytes.includes(Buffer.from(TEST_MODE_MARKER)), false);
});

test("the receipt hash is stable for the same bytes and differs for different bytes", () => {
  const root = tmp();
  const a = join(root, "a"); const b = join(root, "b");
  writeFileSync(a, Buffer.concat([PE, filler(100)]));
  writeFileSync(b, Buffer.concat([PE, filler(101)]));
  const first = scanSwitchPaths([a]).receipt.files[0].sha256;
  assert.equal(scanSwitchPaths([a]).receipt.files[0].sha256, first);
  assert.notEqual(scanSwitchPaths([b]).receipt.files[0].sha256, first);
});

// N7: synthetic native header file containing a deny name (UTF-8 and UTF-16LE) fails; a clean twin passes.
test("N7 deny names in a native binary fail in UTF-8 and UTF-16LE; clean twins pass", () => {
  const name = "HEARDRIGHT_ENGINE_TEST_MODE";
  for (const header of [MACHO, PE]) {
    const utf8 = Buffer.concat([header, filler(200), Buffer.from(name, "latin1"), filler(200)]);
    const utf16 = Buffer.concat([header, filler(200), Buffer.from(name, "utf16le"), filler(200)]);
    const twin = Buffer.concat([header, filler(200), Buffer.from("HEARDRIGHT_ENGINE", "latin1"), filler(200)]);
    for (const bad of [utf8, utf16]) {
      const findings = bufferSwitchFindings(bad);
      assert.deepEqual(ids(findings), ["release-switch-names"]);
      assert.match(findings[0].reason, /HEARDRIGHT_ENGINE_TEST_MODE/);
    }
    assert.deepEqual(bufferSwitchFindings(twin), []);
  }
});

test("a name split across a chunk boundary is still found, and app-declared names extend the registry", () => {
  const name = "RIGHTKIT_UNDER_TEST";
  const split = 4 * 1024 * 1024 - 5;
  const big = Buffer.concat([MACHO, filler(split - 4), Buffer.from(name), filler(100)]);
  assert.deepEqual(ids(bufferSwitchFindings(big)), ["release-switch-names"]);

  const custom = Buffer.concat([MACHO, filler(50), Buffer.from("APPONLY_DATA_DIR"), filler(50)]);
  assert.deepEqual(bufferSwitchFindings(custom), []);
  const withApp = denyNamesFor({ appForbid: ["APPONLY_DATA_DIR"] });
  assert.deepEqual(ids(bufferSwitchFindings(custom, { denyMap: withApp })), ["release-switch-names"]);
});

test("non-native files are not scanned and requireBinary fails an empty scan", () => {
  const root = tmp();
  writeFileSync(join(root, "notes.txt"), "RIGHTKIT_UNDER_TEST");
  assert.equal(scanSwitchPaths([root]).receipt.binaries, 0);
  const { findings } = releaseSwitchFindings({ binaryPaths: [root] });
  assert.deepEqual(ids(findings), ["unscannable"]);
});

// N8: source inventory completeness.
test("N8 an unregistered env read in app source fails the completeness walker; the registered fixture app passes", () => {
  const appRoot = new URL("app/", FIX).pathname;
  assert.deepEqual(sourceSwitchFindings({ sourceRoot: appRoot, inventoryText: fixture("app/switches.json") }), []);

  const root = tmp();
  mkdirSync(join(root, "src"));
  writeFileSync(join(root, "src", "main.rs"), 'fn main() {\n    let _ = std::env::var("X_DATA_DIR");\n}\n');
  const findings = sourceSwitchFindings({ sourceRoot: root, inventoryText: fixture("app/switches.json") });
  assert.deepEqual(ids(findings), ["unregistered-read"]);
  assert.match(findings[0].file, /src[\\/]main\.rs:2/);
  assert.match(findings[0].reason, /X_DATA_DIR/);
});

test("N8 walker covers other languages, dynamic reads and deny-listed flags", () => {
  const root = tmp();
  mkdirSync(join(root, "src"));
  writeFileSync(join(root, "src", "a.ts"), 'const a = process.env.FOO_ROOT; const b = import.meta.env.VITE_APP_QA; const m = import.meta.env.MODE;\n');
  writeFileSync(join(root, "src", "b.rs"), 'fn f(k: &str) { let _ = std::env::var(k); }\nfn g() { let _ = args.any(|a| a == "--qa"); }\n');
  writeFileSync(join(root, "src", "c.swift"), 'let x = ProcessInfo.processInfo.environment["SW_ROOT"]\n');
  writeFileSync(join(root, "src", "ignored.test.ts"), "process.env.TEST_ONLY_NAME\n");
  const inv = JSON.stringify({ schema: 1, app: "x", switches: [] });
  const got = sourceSwitchFindings({ sourceRoot: root, inventoryText: inv });
  const reasons = got.map((f) => f.reason).join("\n");
  for (const expected of ["FOO_ROOT", "VITE_APP_QA", "SW_ROOT", "--qa"]) assert.match(reasons, new RegExp(expected.replace("-", "\\-")));
  assert.ok(ids(got).includes("dynamic-read"));
  assert.doesNotMatch(reasons, /TEST_ONLY_NAME|MODE`/);

  const allowed = JSON.stringify({ schema: 1, app: "x", switches: [], dynamicReads: [{ file: "src/b.rs", reason: "fixture" }] });
  assert.equal(ids(sourceSwitchFindings({ sourceRoot: root, inventoryText: allowed })).includes("dynamic-read"), false);
});

test("inventory schema: S1-S4 must be compiled out, S5 needs a user gesture, S6 a bounded type", () => {
  const entry = (extra) => JSON.stringify({ schema: 1, app: "x", switches: [{ name: "N", channel: "env", ...extra }] });
  assert.deepEqual(parseInventory(entry({ class: "S4", compiledOut: "feature:test-mode" })).issues, []);
  for (const cls of ["S1", "S2", "S3", "S4"]) {
    assert.deepEqual(ids(parseInventory(entry({ class: cls })).issues), ["not-compiled-out"], cls);
    assert.deepEqual(ids(parseInventory(entry({ class: cls, compiledOut: "no" })).issues), ["not-compiled-out"], cls);
  }
  assert.deepEqual(ids(parseInventory(entry({ class: "S5" })).issues), ["inventory-invalid"]);
  assert.deepEqual(parseInventory(entry({ class: "S5", userGesture: true })).issues, []);
  assert.deepEqual(ids(parseInventory(entry({ class: "S6", valueType: "string" })).issues), ["inventory-invalid"]);
  assert.deepEqual(parseInventory(entry({ class: "S6", valueType: "bool" })).issues, []);
  assert.deepEqual(ids(parseInventory(entry({ class: "S9" })).issues), ["inventory-invalid"]);
  assert.deepEqual(ids(parseInventory("{").issues), ["inventory-invalid"]);
});

test("a clean feature tree, clean command and missing optional layers produce no findings", () => {
  assert.deepEqual(featureClosureFindings(fixture("tree-clean.txt")), []);
  assert.deepEqual(commandSwitchFindings(["cargo", "build", "--release", "--features", "custom-protocol"]), []);
  assert.deepEqual(releaseSwitchFindings({}).findings, []);
});

test("closure: resolveFeatureClosure runs cargo tree for the target, forwards features, and fails closed", () => {
  const root = tmp();
  mkdirSync(join(root, "src-tauri"));
  writeFileSync(join(root, "src-tauri", "Cargo.toml"), "[package]\nname=\"x\"\n");
  let seen;
  const ok = (command, args) => { seen = { command, args }; return { status: 0, stdout: fixture("tree-with-test-mode.txt"), stderr: "" }; };
  const got = resolveFeatureClosure({ root, triple: "aarch64-apple-darwin", commandTexts: ["cargo build --release --no-default-features --features custom-protocol"], run: ok });
  assert.equal(seen.command, "cargo");
  for (const a of ["tree", "features,no-dev", "--locked", "aarch64-apple-darwin", "--no-default-features", "custom-protocol"]) assert.ok(seen.args.includes(a), a);
  assert.ok(featureClosureFindings(got.treeText).length >= 1);
  assert.throws(() => resolveFeatureClosure({ root, run: () => ({ status: 101, stdout: "", stderr: "lock needs update" }) }), /feature closure.*failed/s);
  assert.throws(() => resolveFeatureClosure({ root, run: () => ({ status: null, error: new Error("ENOENT") }) }), /ENOENT/);
  assert.throws(() => resolveFeatureClosure({ root, run: () => ({ status: 0, stdout: "  " }) }), /printed nothing/);
  assert.throws(() => resolveFeatureClosure({ root: tmp() }), /release-switch-manifest-missing/);
});

test("closure is the authoritative gate: a clean binary with test-mode in the tree is still refused", () => {
  const { findings } = releaseSwitchFindings({ treeText: fixture("tree-with-test-mode.txt") });
  assert.ok(ids(findings).includes("forbidden-feature test-mode"));
  assert.throws(() => assertNoReleaseSwitches({ treeText: fixture("tree-with-test-mode.txt") }), /forbidden-feature test-mode/);
});

test("the binary scan refuses the app's own S1-S4 names from switches.json, not S5/S6", () => {
  const inventoryText = JSON.stringify({ schema: 1, app: "x", switches: [
    { name: "APPX_FAKE_KEYCHAIN", class: "S2", channel: "env", compiledOut: "feature:test-mode" },
    { name: "APPX_PICKED_DIR", class: "S5", channel: "env", userGesture: true },
  ] });
  assert.deepEqual(inventoryDenyNames(inventoryText), [{ name: "APPX_FAKE_KEYCHAIN", class: "S2" }]);
  const root = tmp();
  const bin = join(root, "app");
  writeFileSync(bin, Buffer.concat([MACHO, filler(64), Buffer.from("APPX_FAKE_KEYCHAIN"), filler(64)]));
  assert.deepEqual(ids(releaseSwitchFindings({ binaryPaths: [bin], inventoryText }).findings), ["release-switch-names"]);
  writeFileSync(bin, Buffer.concat([MACHO, filler(64), Buffer.from("APPX_PICKED_DIR"), filler(64)]));
  assert.deepEqual(releaseSwitchFindings({ binaryPaths: [bin], inventoryText }).findings, []);
});

test("source walker catches imported var()/var_os(), env::var_os and clap env attributes", () => {
  const root = tmp();
  mkdirSync(join(root, "src"));
  writeFileSync(join(root, "src", "a.rs"), 'use std::env::var;\nfn f() { let _ = var("IMPORTED_ONE"); }\n');
  writeFileSync(join(root, "src", "b.rs"), 'use std::env::{self, var_os};\nfn f() { let _ = var_os("IMPORTED_TWO"); let _ = env::var_os("QUALIFIED_OS"); }\n');
  writeFileSync(join(root, "src", "c.rs"), '#[derive(Parser)]\nstruct A {\n  #[arg(long, env = "CLAP_ENV_NAME")]\n  x: String,\n}\nfn g() { Arg::new("y").env("CLAP_BUILDER"); }\n');
  writeFileSync(join(root, "src", "d.rs"), 'fn var(_: &str) {}\nfn h() { var("NOT_AN_ENV_READ"); }\n');
  const reasons = sourceSwitchFindings({ sourceRoot: root, inventoryText: JSON.stringify({ schema: 1, app: "x", switches: [] }) }).map((f) => f.reason).join("\n");
  for (const name of ["IMPORTED_ONE", "IMPORTED_TWO", "QUALIFIED_OS", "CLAP_ENV_NAME", "CLAP_BUILDER"]) assert.match(reasons, new RegExp(name), name);
  assert.doesNotMatch(reasons, /NOT_AN_ENV_READ/);
});

test("closure: a missing manifest is a hard failure; only an explicit rust:false skips", () => {
  const bare = tmp();
  assert.throws(() => resolveFeatureClosure({ root: bare }), /release-switch-manifest-missing/);
  assert.throws(() => resolveFeatureClosure({ root: bare, manifest: "engine/Cargo.toml" }), /release-switch-manifest-missing.*engine\/Cargo.toml/s);
  assert.throws(() => resolveFeatureClosure({ root: bare, rust: true }), /release-switch-manifest-missing/);
  assert.match(resolveFeatureClosure({ root: bare, rust: false }).skipped, /rust is false/);
  const root = tmp();
  mkdirSync(join(root, "engine"));
  writeFileSync(join(root, "engine", "Cargo.toml"), "[package]\nname=\"x\"\n");
  let seen;
  resolveFeatureClosure({ root, cargoManifest: "engine/Cargo.toml", run: (c, a) => { seen = a; return { status: 0, stdout: "x v1\n", stderr: "" }; } });
  assert.ok(seen.includes(join(root, "engine/Cargo.toml")));
});

test("closure: features come from package.json scripts a hook runs, and from releaseSwitches.features", () => {
  const root = tmp();
  writeFileSync(join(root, "Cargo.toml"), "[package]\nname=\"x\"\n");
  writeFileSync(join(root, "package.json"), JSON.stringify({ scripts: { "tauri:build": "tauri build -- --features shell,test-mode" } }));
  let seen;
  const run = (c, a) => { seen = a; return { status: 0, stdout: "x v1\n", stderr: "" }; };
  resolveFeatureClosure({ root, commandTexts: ["pnpm run tauri:build"], features: ["extra"], run });
  const joined = seen[seen.indexOf("--features") + 1].split(",");
  for (const f of ["shell", "test-mode", "extra"]) assert.ok(joined.includes(f), f);
});

test("closure: a --no-default-features command and a default-feature command are each checked; test-mode in the default graph fails the gate", () => {
  const root = tmp();
  writeFileSync(join(root, "Cargo.toml"), '[package]\nname = "x"\n[features]\ndefault = ["test-mode"]\ntest-mode = []\n');
  const calls = [];
  // Models cargo: the default feature set (which reaches test-mode in this manifest) applies unless --no-default-features is passed.
  const run = (command, args) => {
    calls.push(args);
    const defaults = !args.includes("--no-default-features");
    return { status: 0, stdout: fixture(defaults ? "tree-with-test-mode.txt" : "tree-clean.txt"), stderr: "" };
  };
  const mixed = resolveFeatureClosure({
    root, scripts: {}, triple: "aarch64-apple-darwin",
    commandTexts: ["cargo build --release --no-default-features --features custom-protocol", "cargo build --release --features custom-protocol"],
    run,
  });
  assert.equal(calls.length, 2);
  assert.equal(calls.filter((a) => a.includes("--no-default-features")).length, 1);
  assert.ok(featureClosureFindings(mixed.treeText).length >= 1);
  assert.throws(() => assertNoReleaseSwitches({ treeText: mixed.treeText }), /forbidden-feature test-mode/);

  calls.length = 0;
  const onlyNoDefault = resolveFeatureClosure({ root, scripts: {}, commandTexts: ["cargo build --release --no-default-features"], run });
  assert.equal(calls.length, 1);
  assert.deepEqual(featureClosureFindings(onlyNoDefault.treeText), []);
});

test("closure: identical feature combinations run cargo tree once; feature order and script-expanded flags do not split them", () => {
  const root = tmp();
  writeFileSync(join(root, "Cargo.toml"), '[package]\nname = "x"\n');
  const calls = [];
  const run = (command, args) => { calls.push(args); return { status: 0, stdout: "x v1\n", stderr: "" }; };
  const got = resolveFeatureClosure({
    root, scripts: { "tauri:build": "tauri build --features custom-protocol,shell" },
    commandTexts: ["cargo build --release --features shell,custom-protocol", "pnpm run tauri:build", "cargo build --features custom-protocol --features shell"],
    run,
  });
  assert.equal(calls.length, 1);
  assert.equal(got.trees.length, 1);
  assert.equal(calls[0][calls[0].indexOf("--features") + 1], "custom-protocol,shell");

  calls.length = 0;
  const split = resolveFeatureClosure({ root, scripts: {}, commandTexts: ["cargo build --release", "cargo build --release --no-default-features"], run });
  assert.equal(calls.length, 2);
  assert.equal(split.trees.length, 2);
});

// M7 lane E: rightkit-daemon `remote` is refused in a release graph unless the app declares releaseSwitches.remoteHost: true.
const REMOTE_TREE = 'app v0.0.0 (/work/app)\n└── rightkit-daemon v0.2.0 (/work/rightkit/crates/rightkit-daemon)\n    ├── rightkit-daemon feature "link"\n    └── rightkit-daemon feature "remote"\n';
const LINK_ONLY_TREE = 'app v0.0.0 (/work/app)\n└── rightkit-daemon v0.2.0 (/work/rightkit/crates/rightkit-daemon)\n    └── rightkit-daemon feature "link"\n';
const OTHER_CRATE_TREE = 'app v0.0.0 (/work/app)\n└── other-crate v1.0.0\n    └── other-crate feature "remote"\n';

test("remote feature in a release graph is refused unless the app declares remoteHost true", () => {
  const refused = remoteServiceFindings(REMOTE_TREE);
  assert.deepEqual(ids(refused), ["remote-feature-undeclared"]);
  assert.deepEqual(ids(remoteServiceFindings(REMOTE_TREE, { remoteHost: false })), ["remote-feature-undeclared"]);
  assert.deepEqual(remoteServiceFindings(REMOTE_TREE, { remoteHost: true }), []);
});

test("link-only graphs and other crates' remote features are not refused", () => {
  assert.deepEqual(remoteServiceFindings(LINK_ONLY_TREE), []);
  assert.deepEqual(remoteServiceFindings(OTHER_CRATE_TREE), []);
  assert.deepEqual(remoteServiceFindings(fixture("tree-clean.txt")), []);
});

test("remoteHost that is not a boolean fails closed instead of enabling remote", () => {
  assert.throws(() => remoteServiceFindings(REMOTE_TREE, { remoteHost: "true" }), /releaseSwitches\.remoteHost must be true or false/);
  assert.throws(() => remoteServiceFindings(REMOTE_TREE, { remoteHost: 1 }), /must be true or false/);
});

test("releaseSwitchFindings applies the remote check through the feature-closure layer", () => {
  const refused = releaseSwitchFindings({ treeText: REMOTE_TREE, requireBinary: false });
  assert.deepEqual(ids(refused.findings), ["remote-feature-undeclared"]);
  assert.equal(refused.findings[0].layer, "feature-closure");
  assert.deepEqual(releaseSwitchFindings({ treeText: REMOTE_TREE, remoteHost: true, requireBinary: false }).findings, []);
  assert.deepEqual(releaseSwitchFindings({ treeText: REMOTE_TREE, remoteHost: true, bypass: ["feature-closure"], requireBinary: false }).findings, []);
});

// M23: one registry feature vocabulary. The closure list gains the M23 names and the applicable
// qa-build-features aliases. Conditional devtools names share this registry & require explicit content-only allowance.
const M23_CLOSURE_NAMES = ["testing", "test-seams", "isolated-test-keychain", "demo", "dev-overrides", "qa-probes", "dev-worker-override", "qa-shot", "qa-tools", "dev-control", "bench-native"];

test("M23 featureClosureFindings flags each new registry name in cargo-tree text", () => {
  for (const name of ["test-mode", "qa-native", "dev-native", "qa-control", "dev-inspect", ...M23_CLOSURE_NAMES]) {
    assert.ok(REGISTRY.forbiddenFeatures.includes(name), name);
  }
  for (const name of M23_CLOSURE_NAMES) {
    const tree = `app v0.0.0 (/work/app)\nsome-crate feature "${name}"\n`;
    assert.deepEqual(ids(featureClosureFindings(tree)), [`forbidden-feature ${name}`], name);
  }
  const treeOne = `app v0.0.0 (/work/app)\nsome-crate feature "custom-protocol"\n`;
  assert.deepEqual(featureClosureFindings(treeOne), []);
});

test("M23 commandSwitchFindings flags each new registry name in build-command spelling", () => {
  for (const name of M23_CLOSURE_NAMES) {
    assert.deepEqual(ids(commandSwitchFindings(["cargo", "build", "--release", "--features", name])), [`forbidden-feature ${name}`], name);
    assert.deepEqual(ids(commandSwitchFindings(["cargo", "build", "-F", `some-crate/${name}`])), [`forbidden-feature ${name}`], name);
    assert.deepEqual(ids(commandSwitchFindings(["cargo", "build", `--features=custom-protocol,${name}`])), [`forbidden-feature ${name}`], name);
  }
  assert.deepEqual(commandSwitchFindings(["cargo", "build", "--release", "--features", "custom-protocol"]), []);
});

test("M23 content-only exception is explicit in closure & command gates", () => {
  assert.equal(REGISTRY.forbiddenFeatures.includes("devtools"), false);
  assert.equal(REGISTRY.forbiddenFeatures.includes("content-devtools"), false);
  for (const name of ["devtools", "content-devtools"]) {
    const tree = `app v0.0.0 (/work/app)\nsome-crate feature "${name}"\n`;
    assert.equal(featureClosureFindings(tree).length, 1, name);
    assert.deepEqual(featureClosureFindings(tree, { allowDevtools: true }), [], name);
    assert.equal(commandSwitchFindings(["cargo", "build", "--features", name]).length, 1);
    assert.deepEqual(commandSwitchFindings(["cargo", "build", "--features", name], { allowDevtools: true }), [], name);
  }
});

test("malformed or incomplete inventory is an explicit inventory-invalid finding (parseInventory)", () => {
  for (const text of ["", "{", "null", "{}"]) {
    const { doc, issues } = parseInventory(text);
    assert.ok(issues.length >= 1, JSON.stringify(text));
    assert.ok(issues.every((i) => i.rule === "inventory-invalid" && i.layer === "inventory"), JSON.stringify(text));
    assert.ok(doc === null || !Array.isArray(doc.switches) || text === "{}", JSON.stringify(text));
  }
  assert.ok(ids(parseInventory(JSON.stringify({ schema: 1, app: "x" })).issues).includes("inventory-invalid"));
  assert.ok(ids(parseInventory(JSON.stringify({ schema: 1, app: "x", switches: "no" })).issues).includes("inventory-invalid"));
  const missingName = JSON.stringify({ schema: 1, app: "x", switches: [{ class: "S6", channel: "env", valueType: "bool" }] });
  assert.match(parseInventory(missingName).issues.map((i) => i.reason).join("\n"), /needs a `name`/);
  const duplicate = JSON.stringify({ schema: 1, app: "x", switches: [
    { name: "N", class: "S6", channel: "env", valueType: "bool" },
    { name: "N", class: "S6", channel: "env", valueType: "bool" },
  ] });
  assert.match(parseInventory(duplicate).issues.map((i) => i.reason).join("\n"), /listed twice/);
  const badChannel = JSON.stringify({ schema: 1, app: "x", switches: [{ name: "N", class: "S6", channel: "nope", valueType: "bool" }] });
  assert.match(parseInventory(badChannel).issues.map((i) => i.reason).join("\n"), /`channel` must be one of/);
  const root = tmp();
  assert.deepEqual(ids(sourceSwitchFindings({ sourceRoot: root, inventoryText: "{" })), ["inventory-invalid"]);
});


test("transitive QA control is refused by canonical crate identity, not bare control", () => {
  assert.equal(featureClosureFindings('app v1.0.0\n├── intermediary feature "default"\n│   └── rightkit-qa feature "control"').length, 1);
  assert.deepEqual(featureClosureFindings('other-crate feature "control"'), []);
  assert.equal(commandSwitchFindings(["cargo", "build", "--features", "rightkit-qa?/control"]).length, 1);
});


test("combined release gate preserves content-only allowance while refusing transitive QA control", () => {
  const treeText = 'tauri feature "devtools"\nrightkit-tauri-shell feature "content-devtools"';
  const commands = [["cargo", "build", "--features", "content-devtools"]];
  assert.ok(releaseSwitchFindings({ treeText, commands }).findings.length > 0);
  assert.deepEqual(releaseSwitchFindings({ treeText, commands, devtools: { mode: "content-only" } }).findings, []);
  const bad = releaseSwitchFindings({ treeText: `${treeText}\nrightkit-qa feature "control"`, commands, devtools: { mode: "content-only" } });
  assert.equal(bad.findings.length, 1);
  assert.match(bad.findings[0].reason, /rightkit-qa/);
});
