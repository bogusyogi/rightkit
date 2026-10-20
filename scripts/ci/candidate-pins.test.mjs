import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { sha, loadApprovedArtifacts, checkReceipts } from "./upload.mjs";
import { validateCandidate } from "./reviewed-candidate.mjs";
import { checkQualification, collectCandidatePins, proposalOutput } from "./candidate-pins.mjs";

const SOURCE = "a".repeat(40);
const archiveScanFixture = (unit) => ({ schema: 1, scanner: "publish-cargo/scanCrateForSecrets", archiveSha256: unit.sha256, status: "pass", files: 1, violations: [], problems: [] });
function fixture(t, mutate = () => {}) {
  const temp = fs.mkdtempSync(path.join(os.tmpdir(), "rk-pins-"));
  t.after(() => fs.rmSync(temp, { recursive: true, force: true }));
  const archive = Buffer.from("original package bytes");
  const metadata = Buffer.from('{"name":"rightkit-a","vers":"0.1.0","deps":[]}');
  const unit = { name: "rightkit-a", version: "0.1.0", file: "rightkit-a-0.1.0.crate", sha256: sha("sha256", archive), size: archive.length, metaFile: "rightkit-a-0.1.0.upload.json", metaSha256: sha("sha256", metadata) };
  const manifest = { schema: 1, registry: "crates", commit: SOURCE, runId: "77", runAttempt: "1", units: [unit] };
  const files = { packed: { [unit.file]: archive, [unit.metaFile]: metadata, "manifest.json": Buffer.from(JSON.stringify(manifest)) } };
  for (const host of ["macos", "windows"]) files[host] = { [`verify-${host}.json`]: Buffer.from(JSON.stringify({ schema: 1, kind: "verify", host, registry: "crates", commit: SOURCE, manifestSha256: sha("sha256", files.packed["manifest.json"]), runId: "77", runAttempt: "1", manifestRunId: "77", manifestRunAttempt: "1", allowHostExclusion: [], allowNoTest: [], allowIgnored: [], allowMissingAssets: [], units: [{ ...unit, archiveScan: archiveScanFixture(unit), bytesVerified: true, status: "pass", executed: 2, ignored: 0, skipped: 0, consumer: { verified: true } }] })) };
  const zip = Object.fromEntries(Object.keys(files).map((h) => [h, Buffer.from(`zip ${h}`)]));
  const names = { packed: "publish-packed", macos: "publish-receipt-verify-xcode-27", windows: "publish-receipt-verify-windows-2025" };
  const artifacts = Object.entries(names).map(([host, name], i) => ({ id: 100 + i, name, expired: false, digest: `sha256:${sha("sha256", zip[host])}`, workflow_run: { id: 77, head_sha: SOURCE, head_branch: "main", repository_id: 99, head_repository_id: 99 } }));
  const repository = { id: 99, full_name: "bogusyogi/rightkit", default_branch: "main", private: false };
  const workflow = { id: 5, path: ".github/workflows/publish.yml", state: "active" };
  const run = { id: 77, run_attempt: 1, repository, head_repository: repository, head_sha: SOURCE, workflow_id: 5, path: workflow.path, event: "workflow_dispatch", head_branch: "main", status: "completed", conclusion: "success" };
  const jobs = { total_count: 6, jobs: ["guard", "plan", "pack", "qualified", "verify (xcode-27, 26.8.1)", "verify (windows-2025, 26.8.1)"].map((name, i) => ({ id: i + 1, name, status: "completed", conclusion: "success", run_id: 77, run_attempt: 1, head_sha: SOURCE })) };
  const state = { repository, workflow, run, jobs, files, zip, artifacts, unit, redirect: null, changedRun: false }; mutate(state);
  const requests = []; let latestReads = 0;
  const fetchImpl = async (url, options) => {
    const value = String(url); requests.push({ url: value, options });
    if (value.startsWith("https://proof.blob.core.windows.net/")) return { ok: true, status: 200, arrayBuffer: async () => state.zip.packed };
    const prefix = "https://api.github.com/repos/bogusyogi/rightkit";
    assert.ok(value === prefix || value.startsWith(`${prefix}/`), "fixed GitHub repository only");
    const route = value.slice(prefix.length).replace(/^\//, "");
    let doc;
    if (!route) doc = state.repository;
    else if (route === "actions/workflows/publish.yml") doc = state.workflow;
    else if (route === "actions/runs/77") { latestReads++; doc = latestReads > 1 && state.changedRun ? { ...state.run, run_attempt: 2 } : state.run; }
    else if (route === "actions/runs/77/attempts/1") doc = state.run;
    else if (route === "actions/runs/77/attempts/1/jobs?per_page=100") doc = state.jobs;
    else if (route === "actions/runs/77/artifacts?per_page=100") doc = { total_count: state.artifacts.length, artifacts: state.artifacts };
    else if (/^actions\/artifacts\/\d+$/.test(route)) doc = state.artifacts.find((a) => String(a.id) === route.split("/")[2]);
    else if (/^actions\/artifacts\/\d+\/zip$/.test(route)) {
      const host = ["packed", "macos", "windows"][Number(route.split("/")[2]) - 100];
      if (host === "packed" && state.redirect) return { ok: false, status: 302, headers: { get: () => state.redirect } };
      return { ok: true, status: 200, arrayBuffer: async () => state.zip[host] };
    } else assert.fail(`unexpected evidence route ${route}`);
    return { ok: true, status: 200, json: async () => structuredClone(doc) };
  };
  const spawn = (command, args, options) => {
    assert.equal(command, "unzip", "only archive reader may execute");
    assert.equal(options.env.GITHUB_TOKEN, undefined);
    const host = path.basename(args[1]).replace(/\.zip$/, "");
    return { status: 0, stdout: args[0] === "-Z1" ? Object.keys(state.files[host]).join("\n") : state.files[host][args[2]] };
  };
  const outputDir = path.join(temp, "proposal");
  const env = { RUNNER_TEMP: temp, GITHUB_TOKEN: "read-token-fixture" };
  const collect = (overrides = {}) => collectCandidatePins({ runId: "77", candidateId: "candidate-77", outputDir, env, fetchImpl, spawn, ...overrides });
  return { ...state, temp, outputDir, env, requests, collect };
}

test("one-command collection binds exact bytes & emits unsigned schema 2 proposal", async (t) => {
  const f = fixture(t); const { proposal, proposalFile, report, reportFile } = await f.collect();
  assert.deepEqual(JSON.parse(fs.readFileSync(proposalFile)), proposal);
  const candidate = validateCandidate(proposal.candidates[0]);
  assert.equal(candidate.manifestSha256, sha("sha256", f.files.packed["manifest.json"]));
  assert.equal(candidate.artifacts.windows.receiptSha256, sha("sha256", f.files.windows["verify-windows.json"]));
  assert.deepEqual(candidate.units, ["rightkit-a"]);
  assert.deepEqual(proposal.artifacts, [{ registry: "crates", name: "rightkit-a", version: "0.1.0", sha256: f.unit.sha256 }]);
  assert.ok(loadApprovedArtifacts(proposalFile).problems.length > 0, "proposal cannot authorize upload");
  assert.ok(!JSON.stringify(proposal).includes("reviewer") && !JSON.stringify(proposal).includes("reviewRef"));
  assert.ok(f.requests.every((r) => r.options.method === "GET" && r.options.redirect === "manual"));
  assert.ok(!fs.readFileSync(proposalFile, "utf8").includes(f.env.GITHUB_TOKEN));
  assert.equal(report.publicationAuthorized, false); assert.deepEqual(report.missingProof, []);
  assert.deepEqual(JSON.parse(fs.readFileSync(reportFile)), report);
});

test("historical rows yield unchanged unsigned pins & separate explicit proof deficits", async (t) => {
  const f = fixture(t, (s) => {
    s.jobs.jobs = s.jobs.jobs.filter((job) => job.name !== "qualified"); s.jobs.total_count--;
    for (const host of ["macos", "windows"]) {
      const file = `verify-${host}.json`; const receipt = JSON.parse(s.files[host][file]);
      delete receipt.units[0].archiveScan; s.files[host][file] = Buffer.from(JSON.stringify(receipt));
    }
  });
  const { proposal, proposalFile, report } = await f.collect();
  const candidate = proposal.candidates[0];
  validateCandidate(candidate);
  assert.deepEqual(report.missingProof, [
    { kind: "qualified-job", status: "missing" },
    ...["macos", "windows"].map((host) => ({ kind: "archive-scan", status: "missing", host, unit: f.unit.name, sha256: f.unit.sha256 })),
  ]);
  assert.equal(report.publicationAuthorized, false);
  assert.equal(report.manifestSha256, candidate.manifestSha256);
  assert.equal(report.runAttempt, candidate.runAttempt);
  assert.ok(loadApprovedArtifacts(proposalFile).problems.length > 0);
  for (const host of ["macos", "windows"]) {
    const raw = fs.readFileSync(path.join(f.outputDir, "receipts", host, `verify-${host}.json`));
    assert.deepEqual(raw, f.files[host][`verify-${host}.json`]);
    assert.equal(candidate.artifacts[host].receiptSha256, sha("sha256", raw));
    assert.ok(!Object.hasOwn(JSON.parse(raw).units[0], "archiveScan"));
  }
  const manifest = JSON.parse(f.files.packed["manifest.json"]);
  const strict = checkReceipts({ manifest, manifestSha256: candidate.manifestSha256, receiptsDir: path.join(f.outputDir, "receipts"), allow: candidate.allow, run: { runId: candidate.runId, runAttempt: candidate.runAttempt } });
  assert.equal(strict.filter((problem) => problem.includes("archiveScan proof is missing")).length, 2, "publication validation still refuses missing scans");
});

test("historical collection never hides failed jobs, malformed scans or other receipt defects", async (t) => {
  for (const mutate of [
    (s) => { s.jobs.jobs.find((job) => job.name === "qualified").conclusion = "failure"; },
    (s) => { s.jobs.jobs.push({ ...s.jobs.jobs.find((job) => job.name === "qualified"), id: 100 }); s.jobs.total_count++; },
    (s, r) => { r.units[0].archiveScan = null; },
    (s, r) => { r.units[0].archiveScan = { ...archiveScanFixture(s.unit), status: "fail" }; },
    (s, r) => { delete r.units[0].archiveScan; r.units[0].consumer.verified = false; },
    (s, r) => { delete r.units[0].archiveScan; r.manifestSha256 = "c".repeat(64); },
    (s, r) => { delete r.units[0].archiveScan; r.runAttempt = "2"; },
    (s, r) => { delete r.units[0].archiveScan; r.units.push(structuredClone(r.units[0])); },
  ]) {
    const f = fixture(t, (s) => {
      const mac = JSON.parse(s.files.macos["verify-macos.json"]); delete mac.units[0].archiveScan;
      s.files.macos["verify-macos.json"] = Buffer.from(JSON.stringify(mac));
      const win = JSON.parse(s.files.windows["verify-windows.json"]); mutate(s, win);
      s.files.windows["verify-windows.json"] = Buffer.from(JSON.stringify(win));
    });
    await assert.rejects(f.collect(), /refused/);
    assert.ok(!fs.existsSync(path.join(f.outputDir, "candidate-pin-proposal.json")));
  }
});

test("provenance failures never emit proposals", async (t) => {
  for (const mutate of [
    (s) => { s.repository.private = true; }, (s) => { s.repository.default_branch = "topic"; },
    (s) => { s.run.head_repository = { ...s.repository, id: 66 }; },
    (s) => { s.run.event = "pull_request"; }, (s) => { s.run.path = ".github/workflows/ci.yml"; },
    (s) => { s.run.conclusion = "failure"; }, (s) => { s.workflow.state = "disabled_manually"; },
    (s) => { s.jobs.jobs[0].run_attempt = 2; }, (s) => { s.jobs.jobs[0].head_sha = "b".repeat(40); },
    (s) => { s.jobs.jobs[4].conclusion = "failure"; }, (s) => { s.jobs.jobs[5].name = "verify (ubuntu-24.04, 26.8.1)"; },
    (s) => { s.jobs.total_count++; }, (s) => { s.artifacts[0].expired = true; },
    (s) => { s.artifacts[1].workflow_run.head_repository_id = 66; },
    (s) => { s.artifacts.push(structuredClone(s.artifacts[0])); },
    (s) => { s.zip.packed = Buffer.from("tampered zip"); },
    (s) => { s.files.packed[s.unit.file] = Buffer.from("tampered archive"); },
    (s) => { s.changedRun = true; },
  ]) {
    const f = fixture(t, mutate); await assert.rejects(f.collect(), /refused/);
    assert.ok(!fs.existsSync(path.join(f.outputDir, "candidate-pin-proposal.json")));
  }
});

test("receipt bindings, host, consumers, attempt & exact allowsets remain mandatory", async (t) => {
  for (const mutate of [
    (r) => { r.manifestSha256 = "c".repeat(64); }, (r) => { r.runId = "78"; },
    (r) => { r.runAttempt = "2"; }, (r) => { r.manifestRunAttempt = "2"; },
    (r) => { r.host = "macos"; }, (r) => { r.units[0].consumer.verified = false; },
    (r) => { r.units[0].sha256 = "c".repeat(64); },
    (r) => { r.allowIgnored = ["rightkit-a"]; }, (r) => { delete r.allowNoTest; },
  ]) {
    const f = fixture(t, (s) => {
      const receipt = JSON.parse(s.files.windows["verify-windows.json"]); mutate(receipt);
      s.files.windows["verify-windows.json"] = Buffer.from(JSON.stringify(receipt));
    });
    await assert.rejects(f.collect(), /refused/);
  }
});

test("proposal preserves sealed unit order & matching explicit exception order", async (t) => {
  const f = fixture(t, (s) => {
    const manifest = JSON.parse(s.files.packed["manifest.json"]);
    const second = { ...s.unit, name: "rightkit-b", file: "rightkit-b-0.1.0.crate", metaFile: "rightkit-b-0.1.0.upload.json" };
    manifest.units.unshift(second);
    s.files.packed[second.file] = s.files.packed[s.unit.file];
    s.files.packed[second.metaFile] = s.files.packed[s.unit.metaFile];
    s.files.packed["manifest.json"] = Buffer.from(JSON.stringify(manifest));
    for (const host of ["macos", "windows"]) {
      const name = `verify-${host}.json`; const receipt = JSON.parse(s.files[host][name]);
      receipt.manifestSha256 = sha("sha256", s.files.packed["manifest.json"]);
      receipt.allowIgnored = ["rightkit-b", "rightkit-a"];
      receipt.units = manifest.units.map((unit) => ({ ...unit, archiveScan: archiveScanFixture(unit), status: "allowed-skips", bytesVerified: true, executed: 2, ignored: 1, skipped: 0, consumer: { verified: true } }));
      s.files[host][name] = Buffer.from(JSON.stringify(receipt));
    }
  });
  const { proposal } = await f.collect();
  assert.deepEqual(proposal.candidates[0].units, ["rightkit-b", "rightkit-a"]);
  assert.deepEqual(proposal.candidates[0].allow.ignored, ["rightkit-b", "rightkit-a"]);
  assert.deepEqual(proposal.artifacts.map((a) => a.name), ["rightkit-b", "rightkit-a"]);
});

test("signed artifact redirect receives no token & cannot redirect again", async (t) => {
  const f = fixture(t, (s) => { s.redirect = "https://proof.blob.core.windows.net/archive?signature=fixture"; });
  await f.collect();
  const request = f.requests.find((r) => r.url.startsWith("https://proof."));
  assert.equal(request.options.headers, undefined); assert.equal(request.options.redirect, "error");
  for (const redirect of ["http://proof.blob.core.windows.net/a", "https://evil.example/a", "https://proof.blob.core.windows.net.evil.example/a", "https://user:pass@proof.blob.core.windows.net/a", "https://proof.blob.core.windows.net:444/a"]) {
    const bad = fixture(t, (s) => { s.redirect = redirect; });
    await assert.rejects(bad.collect(), /refused/);
    assert.ok(bad.requests.every((r) => r.url.startsWith("https://api.github.com/")));
  }
});

test("explicit IDs, authentication & fresh scratch destination fail before requests", async (t) => {
  const f = fixture(t);
  for (const overrides of [{ runId: "https://example.test/77" }, { runId: "1/../77" }, { candidateId: "../candidate" }, { env: { RUNNER_TEMP: f.temp } }, { outputDir: path.join(path.parse(f.temp).root, "rk-pins-outside-scratch") }, { outputDir: f.temp }]) await assert.rejects(f.collect(overrides), /refused/);
  assert.equal(f.requests.length, 0);
  assert.throws(() => proposalOutput("relative", f.env), /refused/);
});

test("network errors cannot leak read token", async (t) => {
  const f = fixture(t);
  await assert.rejects(f.collect({ fetchImpl: async () => { throw new Error(f.env.GITHUB_TOKEN); } }), (error) => !error.message.includes(f.env.GITHUB_TOKEN));
});

test("qualification: the Linux verify job must have succeeded exactly when the plan requires Linux", (t) => {
  const f = fixture(t);
  const base = { repository: f.repository, workflow: f.workflow, run: f.run, runId: 77 };
  const withJobs = (extra, linuxRequired) => { const jobs = { total_count: f.jobs.jobs.length + extra.length, jobs: [...f.jobs.jobs, ...extra.map((conclusion, i) => ({ id: 50 + i, name: "verify-linux", status: "completed", conclusion, run_id: 77, run_attempt: 1, head_sha: SOURCE }))] }; return () => checkQualification({ ...base, jobs, ...(linuxRequired === undefined ? {} : { linuxRequired }) }); };
  assert.deepEqual(withJobs([])(), []);
  assert.deepEqual(withJobs(["skipped"])(), []);
  assert.deepEqual(withJobs(["success"], true)(), []);
  for (const bad of [[], ["skipped"], ["failure"], ["cancelled"], ["success", "success"]]) assert.throws(withJobs(bad, true), /Linux verification job must succeed exactly when the plan requires Linux/, JSON.stringify(bad));
  assert.throws(withJobs(["success"], false), /Linux verification job must succeed/);
});
