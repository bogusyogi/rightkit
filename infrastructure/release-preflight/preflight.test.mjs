import test from "node:test";
import assert from "node:assert/strict";
import {
  extractSecretsAndVars,
  splitJobBlocks,
  analyzeJobForFetchDepth,
  extractHookPathReferences,
  touchesWorkflows,
  parseCiCommandLines,
} from "./preflight.mjs";

// --- extractSecretsAndVars -------------------------------------------------

test("extractSecretsAndVars: collects secrets and non-defaulted vars, dedup + sorted", () => {
  const yaml = `
steps:
  - env:
      A: \${{ secrets.APPLE_API_KEY }}
      B: \${{ secrets.APPLE_API_KEY }}
      C: \${{ vars.AZURE_CLIENT_ID }}
`;
  const { secrets, vars, varsWithDefault } = extractSecretsAndVars(yaml);
  assert.deepEqual(secrets, ["APPLE_API_KEY"]);
  assert.deepEqual(vars, ["AZURE_CLIENT_ID"]);
  assert.deepEqual(varsWithDefault, []);
});

test("extractSecretsAndVars: a vars.X || 'default' expression is excluded from vars", () => {
  const yaml = `runs-on: \${{ vars.QUALIFICATION_RUNNER || 'windows-2025' }}`;
  const { vars, varsWithDefault } = extractSecretsAndVars(yaml);
  assert.deepEqual(vars, []);
  assert.deepEqual(varsWithDefault, ["QUALIFICATION_RUNNER"]);
});

test("extractSecretsAndVars: mixed defaulted and non-defaulted vars across expressions", () => {
  const yaml = `
runs-on: \${{ vars.RUNNER_LABEL || 'windows-2025' }}
env:
  X: \${{ vars.AZURE_TENANT_ID }}
`;
  const { vars, varsWithDefault } = extractSecretsAndVars(yaml);
  assert.deepEqual(vars, ["AZURE_TENANT_ID"]);
  assert.deepEqual(varsWithDefault, ["RUNNER_LABEL"]);
});

// --- splitJobBlocks + analyzeJobForFetchDepth ------------------------------

const FIXTURE_WORKFLOW = `
name: release-candidate
jobs:
  admission:
    runs-on: ubuntu-24.04
    steps:
      - run: echo admit
  windows-sign:
    needs: [admission]
    environment: release
    steps:
      - uses: actions/checkout@v7
        with:
          ref: \${{ needs.admission.outputs.source_revision }}
          fetch-depth: 0
      - run: echo sign
  broken-job:
    needs: [admission]
    environment: release
    steps:
      - uses: actions/checkout@v7
        with:
          ref: \${{ needs.admission.outputs.source_revision }}
      - run: echo broken
`;

test("splitJobBlocks: finds all top-level jobs under jobs:", () => {
  const jobs = splitJobBlocks(FIXTURE_WORKFLOW);
  assert.deepEqual([...jobs.keys()], ["admission", "windows-sign", "broken-job"]);
});

test("analyzeJobForFetchDepth: detects environment:release + admitted ref + fetch-depth:0", () => {
  const jobs = splitJobBlocks(FIXTURE_WORKFLOW);
  const good = analyzeJobForFetchDepth(jobs.get("windows-sign"));
  assert.equal(good.hasReleaseEnv, true);
  assert.equal(good.hasAdmittedRef, true);
  assert.equal(good.hasFetchDepthZero, true);

  const bad = analyzeJobForFetchDepth(jobs.get("broken-job"));
  assert.equal(bad.hasReleaseEnv, true);
  assert.equal(bad.hasAdmittedRef, true);
  assert.equal(bad.hasFetchDepthZero, false);

  const admission = analyzeJobForFetchDepth(jobs.get("admission"));
  assert.equal(admission.hasReleaseEnv, false);
});

// --- extractHookPathReferences ---------------------------------------------

test("extractHookPathReferences: parses checker= and gate= assignments", () => {
  const hook = `#!/bin/sh
checker="tools/pipelines/hooks/minimize_gate.py"
gate=/absolute/path/to/gate.py
`;
  const refs = extractHookPathReferences(hook);
  assert.ok(refs.includes("tools/pipelines/hooks/minimize_gate.py"));
  assert.ok(refs.includes("/absolute/path/to/gate.py"));
});

test("extractHookPathReferences: ignores comments and picks up bare absolute paths", () => {
  const hook = `#!/bin/sh
# checker=ignored/comment/path.py
node D:\\Claude\\tools\\pipelines\\hooks\\status.py
`;
  const refs = extractHookPathReferences(hook);
  assert.ok(!refs.includes("ignored/comment/path.py"));
  assert.ok(refs.some((r) => r.includes("status.py")));
});

// --- touchesWorkflows --------------------------------------------------------

test("touchesWorkflows: true when a changed path is under .github/workflows/", () => {
  assert.equal(touchesWorkflows(["src/a.ts", ".github/workflows/release-candidate.yml"]), true);
});

test("touchesWorkflows: false when no changed path touches workflows", () => {
  assert.equal(touchesWorkflows(["src/a.ts", "docs/README.md"]), false);
});

// --- parseCiCommandLines --------------------------------------------------

test("parseCiCommandLines: keeps node/pnpm test lines, drops install/cargo/rightkit/comments", () => {
  const script = `#!/usr/bin/env bash
set -euo pipefail

pnpm install --frozen-lockfile
pnpm --dir apps/membrane-hub install --frozen-lockfile
# a comment: node scripts/should-not-run.mjs
cargo test --manifest-path engine/Cargo.toml
rightkit cargo build
pnpm test
pnpm test:random
RIGHT_RELEASE_OFFLINE=1 pnpm --dir apps/membrane-hub test
node --test scripts/release/*.test.mjs
node scripts/ci/check-generated.mjs
`;
  assert.deepEqual(parseCiCommandLines(script), [
    "pnpm test",
    "pnpm test:random",
    "node --test scripts/release/*.test.mjs",
    "node scripts/ci/check-generated.mjs",
  ]);
});

test("parseCiCommandLines: an env-prefixed node line is kept", () => {
  assert.deepEqual(parseCiCommandLines("FOO=bar node run.mjs\n"), ["FOO=bar node run.mjs"]);
});
