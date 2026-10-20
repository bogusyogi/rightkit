import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import test from "node:test";

const SHA = "a".repeat(40);
const HOSTS = ["windows", "macos"];

function fixture() {
  const expected = HOSTS.flatMap((host) => ["test", "closure"].map((steps) => {
    const unit = "rust:rightkit-fixture";
    const command = `cargo ${steps === "test" ? "test" : "check"} -p rightkit-fixture`;
    return { host, steps, artifact: `qa-evidence-${host}-${steps}`, tokens: `${steps === "test" ? "rust" : "rust-check"}:rightkit-fixture`, commands: [{ unit, command }] };
  }));
  const outputs = { evidence: JSON.stringify(expected) };
  const needs = { "private-repo-guard": { result: "success" }, plan: { result: "success", outputs }, hygiene: { result: "success" }, ownership: { result: "success" } };
  for (const host of HOSTS) {
    outputs[`${host}_shard_count`] = "1";
    outputs[`${host}_closure_count`] = "1";
    needs[`shards-${host}`] = { result: "success" };
    needs[`closure-${host}`] = { result: "success" };
  }
  const reports = Object.fromEntries(expected.map((job) => [job.artifact, {
    sha: SHA, host: job.host, selection: job.tokens, steps: job.steps,
    results: job.commands.map((command) => ({ ...command, ok: true, code: 0, timedOut: false, seconds: 1, isTest: job.steps === "test", executed: job.steps === "test" ? 1 : 0, failed: 0, ignored: 0, skipped: 0, binaries: job.steps === "test" ? 1 : 0 })),
    rows: [{ id: job.commands[0].unit, ok: true, known: false, failed: [], seconds: 1, skipped: 0 }],
  }]));
  return { expected, reports, needs };
}

function runSummary(f) {
  const parent = process.env.RIGHTKIT_TEST_SCRATCH ?? (process.env.GITHUB_ACTIONS === "true" ? process.env.RUNNER_TEMP : undefined) ?? (process.platform === "win32" ? "D:/scratch" : "/Volumes/D/scratch");
  const scratch = fs.mkdtempSync(path.join(parent, "rk-compile-summary-"));
  try {
    for (const [artifact, report] of Object.entries(f.reports)) {
      const dir = path.join(scratch, artifact);
      fs.mkdirSync(dir);
      fs.writeFileSync(path.join(dir, "summary.json"), JSON.stringify(report));
    }
    const output = path.join(scratch, "outputs");
    fs.writeFileSync(output, "");
    const run = spawnSync(process.execPath, [fileURLToPath(new URL("./ci-summary.mjs", import.meta.url))], {
      encoding: "utf8", windowsHide: true, timeout: 30_000,
      env: { ...process.env, NEEDS: JSON.stringify(f.needs), GITHUB_SHA: SHA, GITHUB_OUTPUT: output, EVIDENCE_DIR: scratch },
    });
    assert.ifError(run.error);
    return { ...run, green: Object.fromEntries(fs.readFileSync(output, "utf8").trim().split("\n").map((line) => line.split("="))) };
  } finally { fs.rmSync(scratch, { recursive: true, force: true }); }
}

function assertVerdict(f, status, windows, macos) {
  const run = runSummary(f);
  assert.equal(run.status, status, run.stderr);
  assert.deepEqual(run.green, { windows_green: String(windows), macos_green: String(macos) });
  return run;
}

test("exact successful reports permit aggregate & both compile markers", () => {
  assertVerdict(fixture(), 0, true, true);
});

test("ownership remains mandatory for aggregate but does not invalidate compile reports", () => {
  for (const result of ["failure", "cancelled", "skipped", undefined]) {
    const f = fixture();
    if (result === undefined) delete f.needs.ownership;
    else f.needs.ownership.result = result;
    const run = assertVerdict(f, 1, true, true);
    assert.match(run.stderr, /ownership:/);
  }
});

test("shared admission failure blocks both compile markers", () => {
  for (const name of ["private-repo-guard", "plan", "hygiene"]) {
    const f = fixture();
    f.needs[name].result = "failure";
    assertVerdict(f, 1, false, false);
  }
});

test("failed, cancelled, skipped or missing host jobs block only that host marker", () => {
  for (const host of HOSTS) for (const family of ["shards", "closure"]) for (const result of ["failure", "cancelled", "skipped", undefined]) {
    const f = fixture();
    if (result === undefined) delete f.needs[`${family}-${host}`];
    else f.needs[`${family}-${host}`].result = result;
    assertVerdict(f, 1, host !== "windows", host !== "macos");
  }
});

test("missing, failed, known-red & inexact reports block only their host marker", () => {
  for (const host of HOSTS) for (const steps of ["test", "closure"]) for (const defect of ["missing", "failed", "known-red", "sha", "command"]) {
    const f = fixture();
    const artifact = `qa-evidence-${host}-${steps}`;
    const report = f.reports[artifact];
    if (defect === "missing") delete f.reports[artifact];
    else if (defect === "sha") report.sha = "b".repeat(40);
    else if (defect === "command") report.results[0].command += " --unexpected";
    else {
      report.results[0].ok = false;
      report.results[0].code = 1;
      report.results[0].failed = 1;
      report.rows[0].ok = false;
      report.rows[0].known = defect === "known-red";
      report.rows[0].failed = [report.results[0].command];
    }
    assertVerdict(f, 1, host !== "windows", host !== "macos");
  }
});
