import assert from "node:assert/strict";
import fs from "node:fs";
import test from "node:test";
import { parseYaml } from "./workflow-yaml.mjs";

const readYaml = (file) => parseYaml(fs.readFileSync(new URL(file, import.meta.url), "utf8"));
const ACTION = readYaml("../../.github/actions/rk-setup/action.yml");
const WORKFLOW = readYaml("../../.github/workflows/ci.yml");
const setup = (job) => job.steps.find((step) => step.uses === "./.github/actions/rk-setup");

test("shared setup keeps install enabled by default and caches only pnpm store", () => {
  assert.equal(ACTION.inputs["install-dependencies"].default, "true");
  const steps = ACTION.runs.steps;
  const install = steps.find((step) => step.run === "pnpm install --frozen-lockfile");
  assert.equal(install.if, "${{ inputs.install-dependencies == 'true' }}");
  const cache = steps.find((step) => step.name === "Cache pnpm store");
  assert.equal(cache.uses, "actions/cache@0400d5f644dc74513175e3cd8d07132dd4860809");
  assert.equal(cache.with.path, "${{ steps.pnpm-store.outputs.path }}");
  assert.equal(cache.with.key, "${{ runner.os }}-pnpm-11.24.0-${{ hashFiles('pnpm-lock.yaml') }}");
  assert.equal(steps.find((step) => step.id === "pnpm-store").run, 'echo "path=$(pnpm store path)" >> "$GITHUB_OUTPUT"');
});

test("CI omits workspace install only from Rust and Swift work", () => {
  const jobs = WORKFLOW.jobs;
  for (const name of ["shards-windows", "shards-macos"]) {
    assert.equal(setup(jobs[name]).with["install-dependencies"], "${{ matrix.kind == 'npm' || matrix.kind == 'root' }}");
  }
  for (const name of ["closure-windows", "closure-macos"]) assert.equal(setup(jobs[name]).with["install-dependencies"], "false");
  assert.equal(setup(jobs.hygiene).with["install-dependencies"], undefined);
  for (const name of ["shards-windows", "shards-macos", "closure-windows", "closure-macos"]) {
    assert.match(setup(jobs[name]).with.rust, /matrix\.rust|true/);
  }
});

test("shared native matrices wait for hygiene while hygiene stays independent", () => {
  const jobs = WORKFLOW.jobs;
  assert.deepEqual(jobs.hygiene.needs, ["plan"]);
  for (const name of ["shards-windows", "shards-macos", "closure-windows", "closure-macos"]) {
    assert.deepEqual(jobs[name].needs, ["plan", "hygiene"]);
  }
  assert.ok(jobs.summary.needs.includes("hygiene"));
  for (const name of ["shards-windows", "shards-macos", "closure-windows", "closure-macos"]) assert.ok(jobs.summary.needs.includes(name));
});
