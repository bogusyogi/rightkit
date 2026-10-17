import assert from "node:assert/strict";
import { spawnSync } from "./background-process.mjs";
import { readdirSync, readFileSync } from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { validateDef, validateTop } from "./fixtures/ownership/schema-check.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
const fixtures = path.join(here, "fixtures", "ownership");
const casesDir = path.join(fixtures, "cases");
const read = (name, file) => JSON.parse(readFileSync(path.join(casesDir, name, file), "utf8"));
const cases = readdirSync(casesDir).sort().map((name) => ({ name, input: read(name, "input.json"), expected: read(name, "expected.json") }));

// RK_OWNERSHIP_PENDING_ENROLLMENT is a reported warning, never a verdict (#262).
const VERDICT_CODES = ["RK_OWNERSHIP_CLEAN", "RK_OWNERSHIP_ADVISORY", "RK_OWNERSHIP_MALFORMED_REPORT", "RK_OWNERSHIP_INVALID_REGISTRATION",
  "RK_OWNERSHIP_POLICY_DOWNGRADE", "RK_OWNERSHIP_INCOMPLETE_COVERAGE", "RK_OWNERSHIP_VIOLATION", "RK_OWNERSHIP_UNVERIFIED", "RK_OWNERSHIP_REPORT_FAILED"];

test("ownership fixtures match the current implementation (generate.mjs --check)", () => {
  const run = spawnSync(process.execPath, [path.join(fixtures, "generate.mjs"), "--check"], { cwd: here, encoding: "utf8", windowsHide: true, timeout: 300_000 });
  assert.equal(run.status, 0, `${run.stdout}\n${run.stderr}`);
});

test("every verdict code is exercised by a verdict case and by a scan or admission case where reachable", () => {
  const byVerdict = new Set(cases.filter((c) => c.input.kind === "verdict").map((c) => c.expected.verdict.code));
  for (const code of VERDICT_CODES) assert.ok(byVerdict.has(code), `no verdict case for ${code}`);
  const byScan = new Set(cases.filter((c) => c.input.kind === "scan" && c.expected.scan.verdict).map((c) => c.expected.scan.verdict.code));
  for (const code of ["RK_OWNERSHIP_CLEAN", "RK_OWNERSHIP_ADVISORY", "RK_OWNERSHIP_INVALID_REGISTRATION", "RK_OWNERSHIP_POLICY_DOWNGRADE", "RK_OWNERSHIP_INCOMPLETE_COVERAGE", "RK_OWNERSHIP_VIOLATION"]) assert.ok(byScan.has(code), `no real-scan case for ${code}`);
  const errors = new Set(cases.filter((c) => c.input.kind === "scan" && c.expected.scan.error).map((c) => c.expected.scan.code));
  for (const code of ["RK_OWNERSHIP_INVALID_INPUTS", "RK_OWNERSHIP_INPUTS_CHANGED", "RK_OWNERSHIP_UNVERIFIED"]) assert.ok(errors.has(code), `no scan error case for ${code}`);
});

test("expected outputs validate against the published JSON Schemas", () => {
  for (const { name, input, expected } of cases) {
    const problems = [];
    if (input.kind === "verdict") problems.push(...validateTop("verdict.schema.json", expected.verdict));
    else if (input.kind === "cache-key") problems.push(...validateDef("verdict.schema.json", "cacheKeyResult", expected));
    else if (input.kind === "scan") {
      problems.push(...validateDef("report.schema.json", "workerOutput", expected.scan), ...validateDef("report.schema.json", "inputsOutput", expected.inputsMode));
      if (expected.scan.report) problems.push(...validateDef("report.schema.json", "report", expected.scan.report));
    } else if (input.kind === "admission") for (const step of expected.steps) problems.push(...validateDef("verdict.schema.json", "admissionResult", step.result));
    assert.deepEqual(problems, [], `${name}: ${problems.slice(0, 5).join("; ")}`);
  }
});

test("report schema rejects malformed reports the verdict rejects", () => {
  // Cross-field rules (x-crossFieldRules) are enforced by the verdict only, not expressible in JSON Schema.
  const crossField = new Set(["verdict-malformed-transitive-path-mismatch"]);
  for (const { name, input } of cases.filter((c) => c.input.kind === "verdict" && c.expected.verdict.code === "RK_OWNERSHIP_MALFORMED_REPORT" && !crossField.has(c.name))) {
    assert.notDeepEqual(validateDef("report.schema.json", "report", input.report), [], `${name} should fail schema validation`);
  }
  for (const { name, input } of cases.filter((c) => c.input.kind === "verdict" && c.expected.verdict.code !== "RK_OWNERSHIP_MALFORMED_REPORT")) {
    const problems = validateDef("report.schema.json", "report", input.report).filter((p) => !/\.covered\[/.test(p));
    assert.deepEqual(problems, [], `${name}: ${problems.slice(0, 3).join("; ")}`);
  }
});

test("cache-key: key order is irrelevant and every other single-binding change gives a distinct key", () => {
  const key = (name) => cases.find((c) => c.name === name).expected.key;
  assert.equal(key("cache-key-field-order-independent"), key("cache-key-field-order-reference"));
  const keys = cases.filter((c) => c.input.kind === "cache-key" && c.expected.key && c.name !== "cache-key-field-order-reference").map((c) => c.expected.key);
  assert.equal(new Set(keys).size, keys.length);
  for (const binding of ["contentDigest", "catalogDigest", "trustDigest", "approvalsDigest", "scannerPolicyDigest", "protectedFloors"]) assert.ok(cases.some((c) => c.name === `cache-key-missing-${binding}`), binding);
});

test("identity cases include scp, ssh, .git, slash, case, port, userinfo, percent-encoding and IDN", () => {
  const names = cases.filter((c) => c.input.kind === "identity").map((c) => c.name).join(" ");
  for (const word of ["scp", "ssh", "trailing-git", "trailing-slash", "github-path-lowercased", "port", "userinfo", "percent-encoded", "idn"]) assert.match(names, new RegExp(word));
});
