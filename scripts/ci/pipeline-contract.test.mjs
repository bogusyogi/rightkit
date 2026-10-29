// docs/PIPELINE-ARCHITECTURE.md section 5 is the invariant table: every row names the tests that enforce it, or its gap.
// This file holds the table to that (a named test that does not exist fails here) and enforces the entry-point half of
// P1 and P4 itself: each place a push, a compile or a qualification starts runs the check first.
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { parseYaml } from "./workflow-yaml.mjs";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");
// A Windows checkout may hold CRLF (core.autocrlf): the table parser splits on "\n" and strips the closing "|".
const read = (rel) => fs.readFileSync(path.join(ROOT, rel), "utf8").replace(/\r\n/g, "\n");
const workflow = (name) => parseYaml(read(`.github/workflows/${name}`));
const STATUSES = new Set(["enforced", "partial", "gap"]);

/** Section 5 rows: { id, invariant, refs: [{ file, fragment }], status, gap }. */
function invariants() {
  const doc = read("docs/PIPELINE-ARCHITECTURE.md");
  const section = doc.split(/^## 5\. Invariants$/m)[1]?.split(/^## /m)[0];
  assert.ok(section, "docs/PIPELINE-ARCHITECTURE.md has no '## 5. Invariants' section");
  return section.split("\n").filter((line) => /^\| P\d+ \|/.test(line)).map((line) => {
    const cells = line.slice(1, -1).split(" | ").map((c) => c.trim());
    assert.equal(cells.length, 5, `invariant row must have 5 cells: ${line.slice(0, 80)}`);
    const [id, invariant, enforced, status, gap] = cells;
    const refs = [...enforced.matchAll(/`([^`]+)` "([^"]+)"/g)].map((m) => ({ file: m[1], fragment: m[2] }));
    return { id, invariant, refs, status, gap };
  });
}

test("every invariant names the tests that enforce it, or its gap", () => {
  const rows = invariants();
  assert.ok(rows.length >= 10, `expected the P1-P10 rows, found ${rows.length}`);
  assert.deepEqual(rows.map((r) => r.id), rows.map((_, i) => `P${i + 1}`), "invariant ids run P1, P2, ... without gaps or repeats");
  for (const row of rows) {
    assert.ok(STATUSES.has(row.status), `${row.id}: status must be enforced, partial or gap, not ${JSON.stringify(row.status)}`);
    if (row.status !== "gap") assert.ok(row.refs.length > 0, `${row.id}: ${row.status} needs at least one \`file\` "test name" reference`);
    if (row.status === "enforced") assert.equal(row.gap, "—", `${row.id}: an enforced invariant has no gap`);
    else assert.ok(row.gap && row.gap !== "—", `${row.id}: a ${row.status} invariant must say what is missing`);
    for (const { file, fragment } of row.refs) {
      assert.ok(fs.existsSync(path.join(ROOT, file)), `${row.id}: ${file} does not exist`);
      assert.ok(read(file).includes(fragment), `${row.id}: ${file} has no test "${fragment}"`);
    }
  }
});

/**
 * Commit and push run no gate and no local hook check (docs/GATE-CONTRACT.md:32); CI checks every push as it always did. What the contract
 * needs of ci.yml, as problems: it triggers on push, the plan job runs scripts/preflight.mjs first, and the hygiene job runs the class A scans.
 * Takes the parsed workflow so a test can remove either step and see this fail.
 */
export function pushCheckProblems(parsed) {
  const problems = [];
  const on = parsed.on ?? parsed.true;
  if (!on || typeof on !== "object" || !("push" in on)) problems.push("ci.yml does not trigger on push");
  const plan = parsed.jobs?.plan?.steps ?? [];
  const preflight = plan.findIndex((s) => s.run?.trim() === "node scripts/preflight.mjs");
  if (preflight < 0) problems.push("the plan job does not run node scripts/preflight.mjs");
  else if (!plan.slice(0, preflight).every((s) => /^actions\/(checkout|setup-node)@/.test(s.uses ?? ""))) problems.push("something other than checkout and node setup runs before preflight in plan");
  const hygiene = parsed.jobs?.hygiene?.steps ?? [];
  if (!hygiene.some((s) => /(?:^|\s)node scripts\/ci\/gate\.mjs --select \S+ --steps scans(?:\s|$)/.test(s.run ?? ""))) problems.push("the hygiene job does not run the class A scans (scripts/ci/gate.mjs --steps scans)");
  if (!(parsed.jobs?.hygiene?.needs ?? []).includes("plan")) problems.push("hygiene does not need plan");
  return problems;
}

test("P1: ci.yml triggers on push, plan runs preflight first and hygiene runs the class A scans; removing either step fails", () => {
  const parsed = workflow("ci.yml");
  assert.deepEqual(pushCheckProblems(parsed), []);
  // Remove the preflight step: the contract fails.
  const noPreflight = structuredClone(parsed);
  noPreflight.jobs.plan.steps = noPreflight.jobs.plan.steps.filter((s) => s.run?.trim() !== "node scripts/preflight.mjs");
  assert.match(pushCheckProblems(noPreflight).join("\n"), /plan job does not run node scripts\/preflight\.mjs/);
  // Remove the class A scans from hygiene: the contract fails.
  const noScans = structuredClone(parsed);
  noScans.jobs.hygiene.steps = noScans.jobs.hygiene.steps.filter((s) => !/--steps scans/.test(s.run ?? ""));
  assert.match(pushCheckProblems(noScans).join("\n"), /hygiene job does not run the class A scans/);
  // Move preflight behind another step, or drop the push trigger: the contract fails.
  const late = structuredClone(parsed);
  late.jobs.plan.steps = [{ run: "echo first" }, ...late.jobs.plan.steps];
  assert.match(pushCheckProblems(late).join("\n"), /before preflight/);
  const noPush = structuredClone(parsed);
  delete noPush.on.push;
  assert.match(pushCheckProblems(noPush).join("\n"), /does not trigger on push/);
  // No local hook is part of it: the tree commits none.
  assert.equal(fs.existsSync(path.join(ROOT, ".githooks")), false, "RightKit commits no git hook (GATE-CONTRACT:32)");
});

/** Every job a job needs, directly or not. */
const needsOf = (jobs, name, seen = new Set()) => {
  for (const dep of [].concat(jobs[name]?.needs ?? [])) if (!seen.has(dep)) { seen.add(dep); needsOf(jobs, dep, seen); }
  return seen;
};
const stepIndex = (steps, predicate) => steps.findIndex(predicate);

test("P1: ci.yml runs preflight first in plan, and every other job needs plan", () => {
  const { jobs } = workflow("ci.yml");
  const steps = jobs.plan.steps;
  const check = stepIndex(steps, (s) => s.run?.trim() === "node scripts/preflight.mjs");
  assert.ok(check >= 0, "plan runs node scripts/preflight.mjs");
  assert.ok(steps.slice(0, check).every((s) => /^actions\/(checkout|setup-node)@/.test(s.uses ?? "")), "only checkout and node setup come before it");
  for (const name of Object.keys(jobs).filter((j) => !["private-repo-guard", "plan"].includes(j))) {
    assert.ok(needsOf(jobs, name).has("plan"), `${name} must need plan`);
  }
});

test("P1: publish.yml runs preflight in scans before installing any tool", () => {
  const { jobs } = workflow("publish.yml");
  const steps = jobs.scans.steps;
  const check = stepIndex(steps, (s) => s.run?.trim() === "node live/scripts/preflight.mjs");
  assert.ok(check >= 0, "scans runs node live/scripts/preflight.mjs");
  assert.ok(steps.slice(0, check).every((s) => /^actions\/(checkout|setup-node)@/.test(s.uses ?? "")), "only checkouts and node setup come before it");
  const scans = stepIndex(steps, (s) => /run-deterministic-scans\.mjs/.test(s.run ?? ""));
  assert.ok(scans > check, "the class A scans run after it");
  for (const name of Object.keys(jobs).filter((j) => !["guard", "scans"].includes(j))) {
    assert.ok(needsOf(jobs, name).has("scans"), `${name} must need scans`);
  }
});

test("P4: ci.yml compiles natively only on a broker dispatch", () => {
  const { jobs } = workflow("ci.yml");
  const native = Object.entries(jobs).filter(([, job]) => /^(windows|macos|xcode)-/.test(String(job["runs-on"] ?? "")));
  assert.ok(native.length >= 4, "the native shard and closure jobs are present");
  for (const [name, job] of native) assert.match(String(job.if ?? ""), /github\.event_name == 'workflow_dispatch'/, `${name} runs natively on push or pull request`);
});
