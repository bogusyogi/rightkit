import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { cargoManifestIssues, commandFeatureIssues } from "./qa-build-features.mjs";
import {
  REGISTRY, TEST_MODE_MARKER, assertNoReleaseSwitches, bufferSwitchFindings, commandSwitchFindings, denyNamesFor,
  featureClosureFindings, inventoryDenyNames, parseInventory, resolveFeatureClosure, releaseSwitchFindings, scanSwitchPaths, sourceSwitchFindings,
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
  const rust = readFileSync(new URL("../../crates/rightkit-service/src/mode.rs", import.meta.url), "utf8");
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
  assert.ok(closure.some((f) => /rightkit-service/.test(f.reason)));

  for (const argv of [["cargo", "build", "--release", "--features", "test-mode"], ["cargo", "build", "-F", "rightkit-service/test-mode"], ["cargo", "build", "--features=custom-protocol,test-mode"], ["cargo", "build", "-Ftest-mode"]]) {
    assert.deepEqual(ids(commandSwitchFindings(argv)), ["forbidden-feature test-mode"], argv.join(" "));
  }
  assert.deepEqual(ids(commandSwitchFindings(["cargo", "build", "--all-features"])), ["forbidden-feature all-features"]);

  // The existing source-layer scanners agree (right-release already calls these).
  assert.ok(commandFeatureIssues({ cmd: "cargo", args: ["build", "--features", "test-mode"] }, { label: "prePackage" }).length > 0);
  const manifest = fixture("app/Cargo.toml");
  assert.deepEqual(cargoManifestIssues(manifest, "fixture"), []);
  const enabledByDefault = manifest.replace('default = []', 'default = ["test-mode"]');
  assert.ok(cargoManifestIssues(enabledByDefault, "fixture").some((i) => /test-mode/.test(i)));
  const dep = `${manifest}\n[dependencies]\nrightkit-service = { version = "0.1", features = ["test-mode"] }\n`;
  assert.ok(cargoManifestIssues(dep, "fixture").some((i) => /rightkit-service.*test-mode/.test(i)));
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
  assert.deepEqual(resolveFeatureClosure({ root: tmp() }), { skipped: "no Cargo.toml" });
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
