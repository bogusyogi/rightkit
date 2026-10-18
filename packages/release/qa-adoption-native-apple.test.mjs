import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { evaluateQaAdoption, nativeAppleUiTestLanes } from "./qa-contract.mjs";

// A native iOS/macOS app has no package.json or Cargo.toml to pin RightKit QA in; its XCUITest target is its QA lane.
const none = { exempt: null, versions: { npm: {}, cargo: {} }, manifests: [] };
const app = () => mkdtempSync(path.join(os.tmpdir(), "rk-qa-apple-"));

test("an XcodeGen ui-testing target is a native QA lane", () => {
  const root = app();
  writeFileSync(path.join(root, "project.yml"), "targets:\n  AppUITests:\n    type: bundle.ui-testing\n    platform: iOS\n");
  assert.deepEqual(nativeAppleUiTestLanes(root), ["native XCUITest lane (project.yml)"]);
  const result = evaluateQaAdoption({ appRoot: root, app: "Fixture" }, none);
  assert.deepEqual(result.refusals, []);
});

test("an xcodeproj with a ui-testing product is a native QA lane", () => {
  const root = app();
  mkdirSync(path.join(root, "App.xcodeproj"));
  writeFileSync(path.join(root, "App.xcodeproj", "project.pbxproj"), 'productType = "com.apple.product-type.bundle.ui-testing";\n');
  assert.deepEqual(nativeAppleUiTestLanes(root), ["native XCUITest lane (App.xcodeproj)"]);
});

test("a native app with only unit tests still has no QA lane", () => {
  const root = app();
  writeFileSync(path.join(root, "project.yml"), "targets:\n  AppTests:\n    type: bundle.unit-test\n");
  const result = evaluateQaAdoption({ appRoot: root, app: "Fixture" }, none);
  assert.equal(result.refusals.length, 1);
  assert.match(result.refusals[0], /no XCUITest target/);
});
