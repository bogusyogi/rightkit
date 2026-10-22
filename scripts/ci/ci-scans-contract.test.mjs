import assert from "node:assert/strict";
import fs from "node:fs";
import test from "node:test";
import { assertCiScansDoc, MUTATIONS, proveCiScanMutations } from "./ci-scans-contract.mjs";
import { parseYaml } from "./workflow-yaml.mjs";

const doc = () => parseYaml(fs.readFileSync(new URL("../../.github/workflows/ci.yml", import.meta.url), "utf8"));

test("ci.yml satisfies the class A scan contract", () => {
  assertCiScansDoc(doc());
});

test("every scan-contract mutation is rejected", () => {
  assert.ok(MUTATIONS.length >= 20);
  assert.deepEqual(proveCiScanMutations(doc()), []);
});

test("the two rules fail loudly on their own", () => {
  const noScans = doc();
  noScans.jobs.hygiene.steps = noScans.jobs.hygiene.steps.filter((s) => !/--steps scans/.test(s.run ?? ""));
  assert.throws(() => assertCiScansDoc(noScans), /exactly one gate\.mjs --steps scans/);
  const noNeeds = doc();
  noNeeds.jobs["shards-windows"].needs = ["plan"];
  assert.throws(() => assertCiScansDoc(noNeeds), /shards-windows must need hygiene/);
});

test("a gate step with no --steps (default test and closure) outside the hygiene-gated jobs is rejected", () => {
  const stray = doc();
  stray.jobs.plan.steps = [...(stray.jobs.plan.steps ?? []), { run: "node scripts/ci/gate.mjs --select x" }];
  assert.throws(() => assertCiScansDoc(stray), /plan runs class B gate steps/);
});

test("gate.mjs treats a missing scan proof as a failure, never a pass", () => {
  const gate = fs.readFileSync(new URL("./gate.mjs", import.meta.url), "utf8");
  assert.match(gate, /let scansPassed = false;/);
  const shards = fs.readFileSync(new URL("./shards.mjs", import.meta.url), "utf8");
  assert.match(shards, /export const STEPS = Object\.freeze\(\["lock", "fmt", "test", "closure"\]\);/, "scans is not a default step");
  assert.match(gate, /class A scans: no scans step here and no successful hygiene result/);
});

test("CI qualification retains warnings-denied rustdoc for configured feature rows", () => {
  const gate = fs.readFileSync(new URL("./gate.mjs", import.meta.url), "utf8");
  const rustGate = gate.slice(gate.indexOf("async function rustGate("), gate.indexOf("async function npmGate("));
  assert.match(rustGate, /if \(!applies\(name\)\) continue;/);
  assert.match(rustGate, /new Map\(\[\.\.\.featureSets, \.\.\.checkSets\]/);
  assert.match(rustGate, /for \(const features of docSets\) await run\(`rust:\$\{name\}`, "cargo", \["doc", "-p", name, "--no-deps", \.\.\.features\]/);
  assert.match(rustGate, /RUSTDOCFLAGS: `\$\{process\.env\.RUSTDOCFLAGS \?\? ""\} -D warnings`/);
});

test("workflow YAML: the empty flow mapping parses; any other flow mapping is refused; every workflow parses", () => {
  assert.deepEqual(parseYaml("permissions: {}\n"), { permissions: {} });
  assert.throws(() => parseYaml("permissions: { contents: read }\n"), /flow mappings are not supported/);
  for (const f of fs.readdirSync(".github/workflows").filter((n) => /\.ya?ml$/.test(n))) {
    assert.doesNotThrow(() => parseYaml(fs.readFileSync(`.github/workflows/${f}`, "utf8")), f);
  }
});
