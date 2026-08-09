// F5 regression: "every generated workflow's first job carries the
// private-repo guard" is not the same claim as "no job in a generated
// workflow can ever run in a private repo." The QA report proved the two
// diverge in publish-package-managers.yml: `winget-token` had no guard of
// its own and no `needs`, so it always ran (and always exited 0) regardless
// of repository visibility, and `winget` depended only on that unguarded
// job — so both could execute after a repo flipped private, contrary to
// invariant I1 ("no managed workflow ever runs in a private repo").
//
// This test renders every lane and walks the job graph, asserting each job
// is either directly guarded (a job-level `if` that references
// `!github.event.repository.private`) or transitively guarded: it `needs`
// at least one (transitively) guarded job, and does not override the
// default `needs`-implies-`success()` gate with `always()` (which would
// make it run even after a guarded dependency is skipped). This models
// real GitHub Actions semantics: an `if:` expression that does not itself
// call success()/always()/cancelled()/failure() is implicitly ANDed with
// `success()`, so a job whose only dependency was skipped is skipped too.
//
// Fails against the pre-fix templates (winget-token: neither guarded nor
// depending on a guarded job) and passes after F5's fix (winget-token
// carries its own guard).

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
  lanes: [...KNOWN_LANES],
  matrix: { os: ["ubuntu-latest", "macos-latest", "windows-latest"], node: ["22.13", "24"] },
  packageManager: "pnpm@11.18.0",
  publish: {
    npm: { oidc: true, provenance: true },
    homebrew: { tap: "acme/homebrew-tap", formulaName: "widgets" },
    winget: { packageIdentifier: "Acme.Widgets" },
  },
  templateVersion: "1.0.0",
};

const PRIVATE_GUARD_RE = /!\s*github\.event\.repository\.private/;
const ALWAYS_RE = /\balways\(\)/;

/**
 * Minimal line-based parser for the one shape our templates use: top-level
 * `jobs:` map, each job a `  name:` header, job-level `if:`/`needs:` at
 * exactly 4-space indent (steps and their own `if:` live deeper, at 6/8
 * spaces — see templates/*.yml). Good enough for our own generated YAML;
 * not a general YAML parser.
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

    jobs.set(name, { ifExpr: ifLine ? ifLine.trim().slice("if: ".length) : null, needs });
  }
  return jobs;
}

/** True if `jobName` can never execute (nor complete "success") in a private repo. */
function isGuarded(jobs, jobName, visiting = new Set()) {
  if (visiting.has(jobName)) return false; // cycle guard, shouldn't happen
  visiting.add(jobName);

  const job = jobs.get(jobName);
  assert.ok(job, `needs references unknown job '${jobName}'`);

  if (job.ifExpr && PRIVATE_GUARD_RE.test(job.ifExpr)) return true;

  // A custom `if:` that calls always() bypasses the default needs->success()
  // gate, so a guarded dependency being skipped would NOT stop this job.
  if (job.ifExpr && ALWAYS_RE.test(job.ifExpr)) return false;

  // Default GitHub Actions semantics: an `if:` that doesn't call
  // success()/always()/cancelled()/failure() is implicitly ANDed with
  // success(), which requires every direct `needs` job to have succeeded.
  // A skipped (guarded-off) dependency fails that check, so this job is
  // transitively guarded if ANY one of its needs is guarded.
  return job.needs.some((n) => isGuarded(jobs, n, visiting));
}

function renderAllLanes() {
  const context = buildRenderContext(FULL_MANIFEST, "/repos/widgets");
  const rendered = {};
  for (const lane of FULL_MANIFEST.lanes) {
    const templatePath = path.join(templatesDir, LANE_FILENAMES[lane]);
    const templateText = fs.readFileSync(templatePath, "utf8");
    rendered[lane] = renderTemplate(templateText, context);
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
