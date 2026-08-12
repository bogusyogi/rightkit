import test from "node:test";
import assert from "node:assert/strict";
import { admissionBenchmark, catalogScaleChurn, capacityModel, packageBenchmark, parseJson, phase0, stagingPreflight, supplementalPolicyPackage } from "../src/windows-phase0.mjs";
import { report } from "../src/schema.mjs";

test("capacity model reserves protected non-cache classes", () => {
  const model = capacityModel({ volumes: [{ SizeRemaining: 30 * 1024 ** 3 }] });
  assert.equal(model.status, "OBSERVED");
  assert.deepEqual(model.protectedNonCacheClasses, ["runtime", "policy", "receipts", "stage", "vhdx-host"]);
});

test("admission benchmark reports p99 against five-minute SLA", () => {
  const benchmark = admissionBenchmark([100, 200, 301000]);
  assert.equal(benchmark.p99Ms, 301000);
  assert.equal(benchmark.status, "BLOCKED_EXTERNAL");
});

test("staging preflight flags current user-writable entrypoints", () => {
  const preflight = stagingPreflight({ entrypoints: [{ Source: "C:\\Users\\dev\\.cargo\\bin\\cargo.exe" }] });
  assert.equal(preflight.status, "BLOCKED_EXTERNAL");
  assert.equal(preflight.observedUserWritableEntrypoints.length, 1);
});

test("schema rejects a blocker-free blocked report", () => {
  assert.throws(() => report({ outcome: "BLOCKED_EXTERNAL", machine: {}, probes: [] }), /requires blocker/);
});

test("non-Windows invocation produces typed external blocker", async () => {
  const result = await phase0(undefined, "2026-08-12T00:00:00.000Z");
  if (process.platform !== "win32") {
    assert.equal(result.outcome, "BLOCKED_EXTERNAL");
    assert.equal(result.blockers[0].id, "windows-host");
  }
});

test("invalid JSON identifies probe", () => assert.throws(() => parseJson("no", "inventory"), /inventory returned invalid JSON/));

test("2x load input is deterministic and never permits activation", () => {
  const input = catalogScaleChurn({ modules: [{ sha256: "a".repeat(64), kind: "python" }, { sha256: "b".repeat(64), kind: "node" }] });
  assert.equal(input.targetRuleCount, 4);
  assert.equal(input.churnRuleCount, 1);
  assert.equal(input.activationPermitted, false);
  assert.equal(new Set(input.rules.map((rule) => rule.id)).size, 4);
});

test("supplemental package has recoverable non-deployment contract", () => {
  const pkg = supplementalPolicyPackage(catalogScaleChurn({ modules: [{ sha256: "a".repeat(64), kind: "rust" }] }), "2026-08-12T00:00:00.000Z");
  assert.equal(pkg.deployment.mode, "NOT_DEPLOYED");
  assert.equal(pkg.deployment.prohibitedOnCurrentHost, true);
  assert.equal(pkg.rollback.bootRecoveryDrillRequired, true);
});

test("package benchmark does not misrepresent non-sacrificial activation", () => {
  const benchmark = packageBenchmark(catalogScaleChurn({ modules: [] }));
  assert.equal(benchmark.status, "BLOCKED_EXTERNAL");
  assert.equal(benchmark.activationP99Ms, null);
});
