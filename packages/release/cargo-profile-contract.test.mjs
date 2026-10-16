import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import {
  CARGO_PROFILE_TEMPLATE,
  assertCargoProfileContract,
  checkManifestProfiles,
  formatCargoProfileFindings,
  loadCargoProfileTemplate,
  scanCargoProfileContract,
} from "./cargo-profile-contract.mjs";

const template = readFileSync(CARGO_PROFILE_TEMPLATE, "utf8");
const WORKSPACE = '[workspace]\nmembers = ["app"]\nresolver = "2"\n';

function appFixture(files) {
  const root = mkdtempSync(path.join(tmpdir(), "rightkit-profile-contract-"));
  for (const [name, content] of Object.entries(files)) {
    mkdirSync(path.dirname(path.join(root, name)), { recursive: true });
    writeFileSync(path.join(root, name), content, "utf8");
  }
  return root;
}
const withApp = (files, body) => { const root = appFixture(files); try { return body(root); } finally { rmSync(root, { recursive: true, force: true }); } };
const rules = (findings) => findings.map(({ rule }) => rule);

test("the template defines the dev table, the dependency table and release panic", () => {
  const loaded = loadCargoProfileTemplate();
  assert.equal(loaded.dev.panic, "unwind");
  assert.equal(loaded.dev.opt_level, undefined);
  assert.equal(loaded.dev["opt-level"], 1);
  assert.equal(loaded.devPackageStar["codegen-units"], 16);
  assert.equal(loaded.releasePanic, "unwind");
});

test("a workspace root that carries the template verbatim has no findings", () => {
  withApp({ "Cargo.toml": `${WORKSPACE}${template}`, "app/Cargo.toml": '[package]\nname = "app"\nversion = "0.0.0"\n' }, (root) => {
    assert.deepEqual(scanCargoProfileContract(root), []);
    assert.equal(assertCargoProfileContract(root, "fixture"), 0);
  });
});

test("keeping the app's other release keys is allowed when panic is unwind", () => {
  withApp({ "Cargo.toml": `${WORKSPACE}${template.replace('[profile.release]\npanic = "unwind"', '[profile.release]\nopt-level = "s"\nlto = "thin"\npanic = "unwind"')}` }, (root) => {
    assert.deepEqual(scanCargoProfileContract(root), []);
  });
});

test("an app still on release-iterate with abort and a test profile is flagged per rule", () => {
  const legacy = `${WORKSPACE}
[profile.dev]
debug = "line-tables-only"
incremental = true

[profile.release]
panic = "abort"

[profile.release-iterate]
inherits = "release"
opt-level = 1

[profile.release-iterate.package."*"]
incremental = false

[profile.test]
debug = "line-tables-only"
`;
  withApp({ "Cargo.toml": legacy }, (root) => {
    const findings = scanCargoProfileContract(root);
    const found = new Set(rules(findings));
    for (const rule of ["profile-key", "profile-extra", "profile-missing", "panic", "retired-profile"]) assert.ok(found.has(rule), `${rule} expected in ${JSON.stringify(rules(findings))}`);
    const text = formatCargoProfileFindings(findings, "legacy").join("\n");
    assert.match(text, /\[profile\.dev\] is missing panic = "unwind"/);
    assert.match(text, /\[profile\.dev\] is missing opt-level = 1/);
    assert.match(text, /\[profile\.release\] panic must be "unwind" \(found "abort"\)/);
    assert.match(text, /\[profile\.release-iterate\] is retired/);
    assert.match(text, /\[profile\.dev\.package\."\*"\] is missing/);
    assert.match(text, /\[profile\.test\] overrides are not allowed/);
    assert.throws(() => assertCargoProfileContract(root, "legacy"), /legacy breaks the RightKit Cargo profile contract/);
  });
});

test("extra keys, extra dev subtables and a profile inheriting release-iterate are flagged", () => {
  const manifest = {
    profile: {
      dev: { "opt-level": 1, debug: "line-tables-only", strip: "none", "debug-assertions": true, "overflow-checks": true, lto: "off", panic: "unwind", incremental: true, "codegen-units": 256, rpath: false, split_debuginfo: "x", "build-override": { debug: false }, package: { "*": { incremental: false, "codegen-units": 16, debug: false }, foo: { "opt-level": 3 } } },
      release: { panic: "unwind" },
      custom: { inherits: "release-iterate" },
    },
  };
  const messages = checkManifestProfiles(manifest, loadCargoProfileTemplate(), "Cargo.toml").map(({ message }) => message);
  assert.ok(messages.some((m) => /\[profile\.dev\] has split_debuginfo/.test(m)));
  assert.ok(messages.some((m) => /\[profile\.dev\.build-override\] is not in the template/.test(m)));
  assert.ok(messages.some((m) => /\[profile\.dev\.package\."foo"\]/.test(m)));
  assert.ok(messages.some((m) => /\[profile\.dev\.package\."\*"\] has debug/.test(m)));
  assert.ok(messages.some((m) => /\[profile\.custom\] inherits the retired release-iterate/.test(m)));
});

test("a non-compliant sibling shipping workspace fails even when a compliant workspace is shallower", () => {
  withApp({
    "src-tauri/Cargo.toml": `${WORKSPACE}${template}`,
    "engine/Cargo.toml": `${WORKSPACE}[profile.release]\npanic = "abort"\n`,
    "membrane/engine/Cargo.toml": `${WORKSPACE}[profile.release-iterate]\n`,
  }, (root) => {
    const findings = scanCargoProfileContract(root);
    const files = new Set(findings.map(({ file }) => file));
    assert.ok(files.has("engine/Cargo.toml"), `engine workspace expected in ${JSON.stringify([...files])}`);
    assert.ok(files.has("membrane/engine/Cargo.toml"), `membrane workspace expected in ${JSON.stringify([...files])}`);
    assert.ok(!files.has("src-tauri/Cargo.toml"), "the compliant workspace must not be flagged");
    assert.ok(findings.some(({ file, rule }) => file === "engine/Cargo.toml" && rule === "panic"));
    assert.ok(findings.some(({ file, rule }) => file === "membrane/engine/Cargo.toml" && rule === "retired-profile"));
  });
});

test("qa/ and benchmarks/ workspaces are exempt as non-shipping", () => {
  withApp({
    "Cargo.toml": `${WORKSPACE}${template}`,
    "qa/native-e2e/Cargo.toml": `${WORKSPACE}[profile.release]\npanic = "abort"\n`,
    "benchmarks/Cargo.toml": `${WORKSPACE}[profile.release-iterate]\n`,
    "benchmarks/tool/Cargo.toml": '[package]\nname = "tool"\nversion = "0.0.0"\n',
  }, (root) => assert.deepEqual(scanCargoProfileContract(root), []));
});

test("cargoProfileExempt in right-release.config.mjs exempts a declared workspace, and only that one", () => {
  withApp({
    "Cargo.toml": `${WORKSPACE}${template}`,
    "right-release.config.mjs": "export default { schema: 1, app: 'fixture', cargoProfileExempt: ['tools/probe/', \"research/bench\"] };\n",
    "tools/probe/Cargo.toml": `${WORKSPACE}[profile.release]\npanic = "abort"\n`,
    "research/bench/Cargo.toml": `${WORKSPACE}[profile.release]\npanic = "abort"\n`,
    "engine/Cargo.toml": `${WORKSPACE}[profile.release]\npanic = "abort"\n`,
  }, (root) => {
    const files = [...new Set(scanCargoProfileContract(root).map(({ file }) => file))].sort();
    assert.deepEqual(files, ["engine/Cargo.toml", "research/bench/Cargo.toml"]);
  });
});

test("cargoProfileExempt entries must be string literals naming a directory inside the app", () => {
  withApp({
    "Cargo.toml": `${WORKSPACE}${template}`,
    "right-release.config.mjs": "export default { schema: 1, cargoProfileExempt: ['../elsewhere', '.'] };\n",
  }, (root) => {
    const findings = scanCargoProfileContract(root);
    assert.equal(findings.filter(({ rule }) => rule === "invalid-exempt").length, 2);
  });
});

test("an app without [workspace] is checked at its shallowest manifest", () => {
  withApp({ "src-tauri/Cargo.toml": '[package]\nname = "app"\nversion = "0.0.0"\n' }, (root) => {
    assert.ok(rules(scanCargoProfileContract(root)).includes("profile-missing"));
  });
});

test("invalid TOML is reported, not thrown", () => {
  withApp({ "Cargo.toml": "this is = = not toml" }, (root) => assert.deepEqual(rules(scanCargoProfileContract(root)), ["invalid-toml"]));
});

test("cfg_attr(not(debug_assertions), windows_subsystem) is flagged, including a multi-line attribute", () => {
  withApp({
    "Cargo.toml": `${WORKSPACE}${template}`,
    "src-tauri/src/main.rs": '#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]\nfn main() {}\n',
    "other/src/main.rs": '// note\n#![cfg_attr(\n    all(not(debug_assertions), target_os = "windows"),\n    windows_subsystem = "windows"\n)]\nfn main() {}\n',
    "engine/src/main.rs": '#![cfg_attr(windows, windows_subsystem = "windows")]\nfn main() {}\n',
  }, (root) => {
    const findings = scanCargoProfileContract(root).filter(({ rule }) => rule === "windows-subsystem");
    assert.deepEqual(findings.map(({ file, line }) => `${file}:${line}`).sort(), ["other/src/main.rs:2", "src-tauri/src/main.rs:1"]);
  });
});

test("a cfg(debug_assertions)-gated environment override is flagged; a test-mode feature gate and an allow marker are not", () => {
  withApp({
    "Cargo.toml": `${WORKSPACE}${template}`,
    "src/gated.rs": '#[cfg(debug_assertions)]\nfn base() -> Option<String> {\n    std::env::var("APP_BASE_URL").ok()\n}\n',
    "src/inline.rs": 'fn base() -> bool {\n    if cfg!(debug_assertions) { std::env::var_os("APP_TOKEN").is_some() } else { false }\n}\n',
    "src/feature.rs": '#[cfg(feature = "test-mode")]\nfn base() -> Option<String> {\n    std::env::var("APP_BASE_URL").ok()\n}\n',
    "src/allowed.rs": '// rightkit-allow: debug-assertions-env\n#[cfg(debug_assertions)]\nfn base() -> Option<String> {\n    std::env::var("APP_BASE_URL").ok()\n}\n',
    "src/checks_only.rs": '#[cfg(debug_assertions)]\nfn check(value: u32) {\n    assert!(value < 10);\n}\n',
    "tests/skipped.rs": '#[cfg(debug_assertions)]\nfn base() { let _ = std::env::var("X"); }\n',
  }, (root) => {
    const findings = scanCargoProfileContract(root).filter(({ rule }) => rule === "debug-assertions-env");
    assert.deepEqual(findings.map(({ file, line }) => `${file}:${line}`).sort(), ["src/gated.rs:1", "src/inline.rs:2"]);
  });
});
