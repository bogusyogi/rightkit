// E01: the pre-build gates report every independent finding in one run; a gate whose input came from a failed gate is
// BLOCKED (never run on that input); a throw inside a gate is a finding (never a crash, never a pass).
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import { writeFileSync } from "node:fs";
import path from "node:path";
import test from "node:test";
import { evaluatePrebuildGates } from "./prebuild-gates.mjs";
import { tempRoot } from "./test-support/temp-roots.mjs";

const FORBIDDEN_PACKAGE = { cmd: "cargo", args: ["build", "--features", "test-mode"] };
const names = (result) => result.findings.map((finding) => finding.name);
const find = (result, name) => result.findings.find((finding) => finding.name === name);

function appRoot({ viteDevelopment = false } = {}) {
  const root = tempRoot("right-release-gates-");
  if (viteDevelopment) writeFileSync(path.join(root, "vite.config.js"), "export default { resolve: { conditions: ['development'] } };\n");
  return root;
}

const quiet = (run) => {
  const log = console.log;
  console.log = () => {};
  try { return run(); } finally { console.log = log; }
};

test("a clean app yields no finding and keeps the return shape", () => {
  const result = quiet(() => evaluatePrebuildGates({ root: appRoot(), config: { app: "x" }, target: {}, platform: "linux", strict: false }));
  assert.deepEqual(Object.keys(result).sort(), ["devtoolsAllowance", "findings", "inventoryText", "treeText"]);
  assert.deepEqual(result.findings, []);
  assert.equal(result.inventoryText, null);
  assert.equal(result.devtoolsAllowance.mode, "denied");
});

test("two failing independent gates are both reported in one run", () => {
  const root = appRoot({ viteDevelopment: true });
  const result = quiet(() => evaluatePrebuildGates({ root, config: { app: "x" }, target: { package: FORBIDDEN_PACKAGE }, platform: "linux", strict: true }));
  // production-mode (frontend resolves development exports) and build-features (test-mode feature) do not read each other.
  assert.match(find(result, "production-mode")?.detail ?? "", /vite\.config\.js lists a "development" resolve condition/);
  assert.match(find(result, "build-features")?.detail ?? "", /enables feature `test-mode`/);
  // and a third, later gate (the missing switches.json) is still reached.
  assert.match(find(result, "switch-inventory-missing")?.detail ?? "", /switches\.json is missing/);
  assert.ok(result.findings.length >= 3, names(result).join(","));
  // gate order is part of the contract: findings come in build order.
  assert.ok(names(result).indexOf("build-features") < names(result).indexOf("production-mode"));
  assert.ok(names(result).indexOf("production-mode") < names(result).indexOf("switch-inventory-missing"));
});

test("a gate that reads a failed gate's output is BLOCKED by it, not skipped and not run on bad input", () => {
  const root = appRoot({ viteDevelopment: true });
  // iOS scan inputs whose prepare list is unusable: switch-inputs cannot be computed.
  const result = quiet(() => evaluatePrebuildGates({ root, config: { app: "x" }, target: {}, platform: "ios", nativeIos: true, iosSpec: { prepare: "not-a-list" }, strict: true }));
  assert.ok(find(result, "switch-inputs"));
  for (const name of ["build-features", "closure-commands"]) assert.equal(find(result, name)?.detail, "BLOCKED by switch-inputs", name);
  // blocked-ness chains through the gates that consume the blocked gate's output.
  for (const name of ["release-dependencies", "feature-closure", "release-switches"]) assert.equal(find(result, name)?.detail, "BLOCKED by closure-commands", name);
  // an independent gate is neither blocked nor skipped.
  assert.match(find(result, "production-mode")?.detail ?? "", /release frontend is not production-only/);
  assert.doesNotMatch(find(result, "production-mode").detail, /BLOCKED/);
});

test("an exception inside a gate is a finding naming the gate, never a crash and never a pass", () => {
  const root = appRoot({ viteDevelopment: true });
  // path.resolve(root, 5) throws a TypeError inside the switch-inventory gate.
  const result = quiet(() => evaluatePrebuildGates({ root, config: { app: "x", releaseSwitches: { inventory: 5 } }, target: {}, platform: "linux", strict: true }));
  assert.match(find(result, "switch-inventory")?.detail ?? "", /^gate switch-inventory threw TypeError: /);
  // what read the inventory is blocked, not passed
  assert.equal(find(result, "release-switches")?.detail, "BLOCKED by switch-inventory");
  assert.ok(!names(result).includes("switch-inventory-missing"), "an unreadable inventory is not reported as a missing one");
  // the other gates still ran
  assert.ok(find(result, "production-mode"));
});

test("a non-Error throw is still a finding", () => {
  const config = { app: "x", get releaseSwitches() { throw "plain string"; } };
  const result = quiet(() => evaluatePrebuildGates({ root: appRoot(), config, target: {}, platform: "linux", strict: false }));
  assert.ok(result.findings.length > 0);
  assert.ok(result.findings.some((finding) => /threw a non-Error value: plain string/.test(finding.detail)), JSON.stringify(result.findings));
});
