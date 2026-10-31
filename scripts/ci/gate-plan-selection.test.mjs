import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const ROOT = fileURLToPath(new URL("../../", import.meta.url));
const GATE = path.join(ROOT, "scripts", "ci", "gate.mjs");

// Exercise the real entry point in read-only plan mode. No compile or test unit runs.
function gate(args) {
  return spawnSync(process.execPath, [GATE, ...args], {
    cwd: ROOT, encoding: "utf8", timeout: 30_000, maxBuffer: 16 * 1024 * 1024,
    env: { ...process.env, GITHUB_ACTIONS: "false", GITHUB_EVENT_NAME: "pull_request", GITHUB_BASE_REF: "__absent_ci_plan_base__" },
  });
}

function plan(args) {
  const result = gate(["--plan", "--host", "linux", ...args]);
  assert.equal(result.error, undefined);
  assert.equal(result.status, 0, result.stderr || result.stdout);
  return JSON.parse(result.stdout);
}

test("universe lists the real catalogue without a baseline or all-units exception", () => {
  const selected = plan(["--universe"]);
  assert.equal(selected.host, "linux");
  assert.equal(selected.base, null);
  assert.equal(selected.all, true);
  assert.equal(selected.rust.all, true);
  assert.equal(selected.npm.all, true);
  assert.ok(selected.rust.test.length > 0);
  assert.ok(selected.npm.test.includes("packages/license"));
});

test("universe cannot become an execution route through named selection", () => {
  for (const extra of [[], ["--select", "root:scripts"], ["--registry", "npm", "--units", "@rightkit/license"]]) {
    const result = gate(["--universe", ...extra]);
    assert.equal(result.status, 2);
    assert.match(result.stderr, /--universe only lists units with --plan; it never runs them/);
  }
});

test("a real non-global base diff remains bounded", () => {
  const selected = plan(["--base", "HEAD"]);
  assert.equal(selected.all, false);
  assert.equal(selected.changedPaths, 0);
  assert.deepEqual(selected.rust, { all: false, test: [], check: [] });
  assert.deepEqual(selected.npm, { all: false, test: [] });
  assert.deepEqual(selected.root, []);
  assert.deepEqual(selected.swift, []);
});

test("explicit shard and publication plans preserve named package scope", () => {
  for (const args of [["--select", "npm:packages/license"], ["--registry", "npm", "--units", "@rightkit/license"]]) {
    const selected = plan(args);
    assert.equal(selected.all, false);
    assert.deepEqual(selected.rust, { all: false, test: [], check: [] });
    assert.deepEqual(selected.npm, { all: false, test: ["packages/license"] });
    assert.deepEqual(selected.root, []);
    assert.deepEqual(selected.swift, []);
  }
});

test("all-units planning still requires an exception id", () => {
  const result = gate(["--plan", "--all"]);
  assert.equal(result.status, 2);
  assert.match(result.stderr, /--all refused:.*all-units-exception/);
});
