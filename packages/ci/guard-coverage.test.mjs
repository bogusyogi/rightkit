// F5 regression, two properties layered on top of each other:
//
// 1. Reachability (original F5): "every generated workflow's first job
//    carries the private-repo guard" is not the same claim as "no job in a
//    generated workflow can ever run in a private repo." The QA report
//    proved the two diverge in publish-package-managers.yml: `winget-token`
//    had no guard of its own and no `needs`, so it always ran (and always
//    exited 0) regardless of repository visibility, and `winget` depended
//    only on that unguarded job.
//
// 2. Loudness (the re-validation finding): even once every job is
//    reachability-guarded, gating the guarded job itself with a job-level
//    `if: !github.event.repository.private` makes GitHub SKIP that job for
//    a private repo — the nested "refuse" step inside it is then
//    unreachable and never runs. A skip is not a failure: the run can read
//    as "nothing to do" instead of the loud, non-zero failure invariant I1
//    requires. The fix is a single `private-repo-guard` job with NO
//    job-level `if` (so it always executes) whose one step's OWN `if`
//    fires only when private and exits 1 — that fails the job outright.
//    Every real job `needs` it instead of gating itself.
//
// This test renders every lane and walks the job graph, asserting each job
// is either itself a "self-guard" job (property 2: no job-level `if`, so it
// unconditionally runs, plus a step whose own `if` matches the positive
// private check and whose body exits non-zero — i.e. it cannot SUCCEED in a
// private repo, and does not merely skip) or transitively guarded: it
// `needs` at least one (transitively) guarded job, and does not override
// the default `needs`-implies-`success()` gate with `always()` (which would
// make it run even after a guarded dependency failed/was skipped). This
// models real GitHub Actions semantics: an `if:` expression that does not
// itself call success()/always()/cancelled()/failure() is implicitly ANDed
// with `success()`, so a job whose only dependency failed is skipped too.
//
// A job gated the OLD way — job-level `if: !private` — is deliberately NOT
// recognized as guarded here: that shape is exactly the loudness bug, so a
// template that regressed to it must fail this test, not pass it.
//
// Fails against the pre-fix templates (winget-token: neither guarded nor
// depending on a guarded job; every guard job-level-`if` gated instead of
// self-failing) and passes after the fix.

import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { renderTemplate } from "./render.mjs";
import { buildRenderContext, LANE_FILENAMES, KNOWN_LANES } from "./manifest.mjs";

const packageRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)));
const templatesDir = path.join(packageRoot, "templates");

const FULL_MANIFEST = {
  schemaVersion: 1,
  compile: "github-actions-only",
  lanes: [...KNOWN_LANES],
  swift: { check: { runner: "macos-15", xcodeVersion: "16.4", projectDirectory: "ios", spec: "project.yml", project: "Widgets.xcodeproj", scheme: "Widgets", xcodegenVersion: "2.44.1" } },
  regenerate: { script: "regenerate" },
  matrix: { os: ["ubuntu-latest", "macos-latest", "windows-latest"], node: ["22.13", "24"] },
  packageManager: "pnpm@11.24.0",
  publish: {
    npm: { oidc: true, provenance: true },
    homebrew: { tap: "acme/homebrew-tap", formulaName: "widgets" },
    winget: { packageIdentifier: "Acme.Widgets" },
  },
  templateVersion: "1.0.0",
};

const NEGATED_PRIVATE_RE = /!\s*github\.event\.repository\.private/;
const POSITIVE_PRIVATE_RE = /(?<!!\s*)github\.event\.repository\.private/;
const ALWAYS_RE = /\balways\(\)/;
const EXIT_NONZERO_RE = /exit\s+[1-9]/;

/**
 * Minimal line-based parser for the one shape our templates use: top-level
 * `jobs:` map, each job a `  name:` header, job-level `if:`/`needs:` at
 * exactly 4-space indent, steps introduced by `      - ` at 6-space indent
 * with their own `if:`/`run:` nested under that (see templates/*.yml). Good
 * enough for our own generated YAML; not a general YAML parser.
 */
function parseJobs(content) {
  const lines = content.split("\n");
  const jobsIdx = lines.findIndex((l) => /^jobs:\s*$/.test(l));
  assert.ok(jobsIdx !== -1, "rendered workflow has no top-level jobs: map");

  const jobStarts = [];
  for (let i = jobsIdx + 1; i < lines.length; i++) {
    if (/^ {2}\S.*:\s*$/.test(lines[i])) jobStarts.push(i);
  }

  const jobs = new Map();
  for (let idx = 0; idx < jobStarts.length; idx++) {
    const start = jobStarts[idx];
    const end = idx + 1 < jobStarts.length ? jobStarts[idx + 1] : lines.length;
    const name = /^ {2}(\S+):/.exec(lines[start])[1];
    const block = lines.slice(start, end);

    const ifLine = block.find((l) => /^ {4}if: /.test(l));
    const needsLine = block.find((l) => /^ {4}needs: /.test(l));

    let needs = [];
    if (needsLine) {
      const inline = /^ {4}needs:\s*\[(.*)\]\s*$/.exec(needsLine);
      if (inline) {
        needs = inline[1].split(",").map((s) => s.trim()).filter(Boolean);
      } else {
        const bare = /^ {4}needs:\s*(\S+)\s*$/.exec(needsLine);
        if (bare) needs = [bare[1]];
      }
    }

    // Self-guard detection: a STEP-level `if:` (any indent deeper than the
    // job-level 4 spaces already captured above as `ifLine`) that positively
    // references repository.private, with a non-zero `exit` appearing before
    // the next step starts. Such a job cannot reach "success" in a private
    // repo regardless of what its job-level `if` says (here, nothing — the
    // fixed templates give these jobs no job-level `if` at all).
    let selfGuardStep = false;
    for (let i = 0; i < block.length; i++) {
      const isStepIfLine = /^\s+if: /.test(block[i]) && !/^ {4}if: /.test(block[i]);
      if (isStepIfLine && POSITIVE_PRIVATE_RE.test(block[i])) {
        const nextStepOffset = block.slice(i + 1).findIndex((l) => /^ {6}- /.test(l));
        const stepBody = nextStepOffset === -1 ? block.slice(i) : block.slice(i, i + 1 + nextStepOffset);
        if (EXIT_NONZERO_RE.test(stepBody.join("\n"))) selfGuardStep = true;
      }
    }

    jobs.set(name, {
      ifExpr: ifLine ? ifLine.trim().slice("if: ".length) : null,
      needs,
      selfGuardStep,
    });
  }
  return jobs;
}

/** True if `jobName` can never SUCCEED in a private repo (fails outright or skips). */
function isGuarded(jobs, jobName, visiting = new Set()) {
  if (visiting.has(jobName)) return false; // cycle guard, shouldn't happen
  visiting.add(jobName);

  const job = jobs.get(jobName);
  assert.ok(job, `needs references unknown job '${jobName}'`);

  // Self-failing job: no job-level `if` gates it away (it always runs), but
  // one of its own steps refuses (fails) when private. This is the LOUD
  // shape — the job runs and fails, it is never silently skipped.
  if (job.selfGuardStep) return true;

  // The OLD shape (job-level `if: !private`) is deliberately NOT treated as
  // guarded here — gating the whole job away is exactly the "skip instead
  // of fail" bug this suite now guards against. A template that regresses
  // to it must fail this assertion, not pass it via this branch.
  if (job.ifExpr && NEGATED_PRIVATE_RE.test(job.ifExpr) && !job.selfGuardStep) {
    return false;
  }

  // A custom `if:` that calls always() bypasses the default needs->success()
  // gate, so a guarded dependency being skipped would NOT stop this job.
  if (job.ifExpr && ALWAYS_RE.test(job.ifExpr)) return false;

  // Default GitHub Actions semantics: an `if:` that doesn't call
  // success()/always()/cancelled()/failure() is implicitly ANDed with
  // success(), which requires every direct `needs` job to have succeeded.
  // A failed or skipped dependency fails that check, so this job is
  // transitively guarded if ANY one of its needs is guarded.
  return job.needs.some((n) => isGuarded(jobs, n, visiting));
}

function renderAllLanes() {
  const rendered = {};
  for (const lane of FULL_MANIFEST.lanes) {
    const templatePath = path.join(templatesDir, LANE_FILENAMES[lane]);
    const templateText = fs.readFileSync(templatePath, "utf8");
    rendered[lane] = renderTemplate(templateText, buildRenderContext(FULL_MANIFEST, "/repos/widgets", lane));
  }
  return rendered;
}

test("F5: every job in every rendered lane is private-guarded or transitively depends on a guarded job", () => {
  const rendered = renderAllLanes();
  for (const lane of KNOWN_LANES) {
    const jobs = parseJobs(rendered[lane]);
    assert.ok(jobs.size > 0, `lane ${lane} has no jobs`);
    for (const jobName of jobs.keys()) {
      assert.ok(
        isGuarded(jobs, jobName),
        `lane ${lane} job '${jobName}' is neither private-guarded nor depends (transitively) on a job that is — a private repo could execute it (F5)`,
      );
    }
  }
});

// F5 loudness proof (the specific gap the re-validation found): a job that
// is only reachable through a job-level `if: !private` is SKIPPED, not
// FAILED, for a private repo — GitHub reports skipped jobs as neutral, and
// a workflow made entirely of skips/successes shows green, not the loud
// failure invariant I1 demands. This asserts the concrete structural
// precondition for the loud-failure behavior: the `private-repo-guard` job
// itself carries NO job-level `if` (so GitHub always schedules and runs it,
// private or not) and its own step-level `if` positively matches private
// with a non-zero `exit` — so when private, the job runs and then fails.
// Fails against the pre-fix shape (guard logic lived inside a job gated by
// `if: !private`, so the refusing step was itself skipped) and against any
// regression back to that shape.
test("F5: private-repo-guard job has no job-level `if` (always runs) and fails outright when private", () => {
  const rendered = renderAllLanes();
  for (const lane of KNOWN_LANES) {
    const jobs = parseJobs(rendered[lane]);
    const guard = jobs.get("private-repo-guard");
    assert.ok(guard, `lane ${lane} has no private-repo-guard job`);
    assert.equal(
      guard.ifExpr,
      null,
      `lane ${lane} private-repo-guard carries a job-level 'if' (${guard.ifExpr}) — that would let it be SKIPPED instead of run-then-failed for a private repo`,
    );
    assert.ok(
      guard.selfGuardStep,
      `lane ${lane} private-repo-guard has no step that positively checks private and exits non-zero`,
    );
  }
});

test("qualification preserves failing schema evidence as a unique required artifact", () => {
  const template = fs.readFileSync(path.join(templatesDir, "qualification.yml"), "utf8");
  assert.doesNotMatch(template, /\|\| true/);
  assert.match(template, /set -o pipefail[\s\S]*tee qualification-schema\.log/);
  assert.match(template, /uses: actions\/upload-artifact@v4[\s\S]*github\.run_id[\s\S]*github\.run_attempt[\s\S]*if-no-files-found: error/);
});

test("rust-hybrid ci keeps its work behind the public-repository guard", () => {
  const manifest = { ...FULL_MANIFEST, lanes: ["ci"], profile: "rust-hybrid", gate: { path: "scripts/gate.sh" } };
  const template = fs.readFileSync(path.join(templatesDir, "profiles/rust-hybrid/ci.yml"), "utf8");
  const jobs = parseJobs(renderTemplate(template, buildRenderContext(manifest, "/repos/widgets")));
  assert.ok(isGuarded(jobs, "private-repo-guard"));
  assert.ok(isGuarded(jobs, "ci"));
});
