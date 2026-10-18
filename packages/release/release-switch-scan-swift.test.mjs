import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { sourceSwitchFindings } from "./release-switch-scan.mjs";

const read = (name) => `let v = ProcessInfo.processInfo.environment["${name}"]\n`;
const inventory = (switches) => JSON.stringify({ schema: 1, app: "fixture", switches, dynamicReads: [] });

function app() {
  const root = mkdtempSync(path.join(os.tmpdir(), "rk-switch-swift-"));
  for (const dir of ["App", "Tests", "UITests", "AppTests", "AppUITests"]) mkdirSync(path.join(root, dir));
  writeFileSync(path.join(root, "App", "Main.swift"), `#if DEBUG\n${read("APP_UI_QA")}#endif\n`);
  for (const dir of ["Tests", "UITests", "AppTests", "AppUITests"]) writeFileSync(path.join(root, dir, "T.swift"), read(`ONLY_IN_${dir.toUpperCase()}`));
  return root;
}

test("Swift test bundles are not scanned; only the app's own reads must be registered", () => {
  const findings = sourceSwitchFindings({ sourceRoot: app(), inventoryText: inventory([]) });
  assert.deepEqual(findings.map((f) => f.rule), ["unregistered-read"]);
  assert.match(findings[0].reason, /APP_UI_QA/);
});

test("a DEBUG-only Swift test switch is accepted as swift:DEBUG", () => {
  const ok = sourceSwitchFindings({ sourceRoot: app(), inventoryText: inventory([{ name: "APP_UI_QA", class: "S1", channel: "env", compiledOut: "swift:DEBUG" }]) });
  assert.deepEqual(ok, []);
  const missing = sourceSwitchFindings({ sourceRoot: app(), inventoryText: inventory([{ name: "APP_UI_QA", class: "S1", channel: "env" }]) });
  assert.equal(missing[0].rule, "not-compiled-out");
});

test("env! of a Cargo-set name is not a switch; any other compile-time name still is", () => {
  const root = mkdtempSync(path.join(os.tmpdir(), "rk-switch-cargo-"));
  mkdirSync(path.join(root, "src"));
  writeFileSync(path.join(root, "src", "lib.rs"), 'const V: &str = env!("CARGO_PKG_VERSION");\nconst D: &str = env!("CARGO_MANIFEST_DIR");\nconst O: Option<&str> = option_env!("OUT_DIR");\nconst F: Option<&str> = option_env!("APP_BUILD_FLAVOR");\n');
  const findings = sourceSwitchFindings({ sourceRoot: root, inventoryText: inventory([]) });
  assert.deepEqual(findings.map((f) => f.reason.match(/`([A-Z_]+)`/)[1]), ["APP_BUILD_FLAVOR"]);
});

test("the recipe's own build output and fetched dependency checkouts are not scanned", () => {
  const root = app();
  const outputs = [
    [".right-release", "ios", "DerivedData", "SourcePackages", "checkouts", "rightkit", "crates", "x", "src"],
    ["DerivedData", "SourcePackages", "checkouts", "dep"],
    [".build", "checkouts", "dep"],
    [".swiftpm", "xcode"],
  ];
  for (const parts of outputs) {
    mkdirSync(path.join(root, ...parts), { recursive: true });
    writeFileSync(path.join(root, ...parts, "lib.swift"), read("FROM_BUILD_OUTPUT"));
  }
  const findings = sourceSwitchFindings({ sourceRoot: root, inventoryText: inventory([]) });
  assert.deepEqual(findings.map((f) => f.reason).filter((reason) => /FROM_BUILD_OUTPUT/.test(reason)), []);
  assert.match(findings[0].reason, /APP_UI_QA/, "the app's own read is still reported");
});
