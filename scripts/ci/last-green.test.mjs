import assert from "node:assert/strict";
import test from "node:test";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { compatibilityFingerprints, selectLastGreenCandidate, createGitHubActionsClient, resolveLastGreen, EVIDENCE_CONTRACT, WORKFLOW_PATH, COMPATIBILITY_GROUPS } from "./last-green.mjs";

const repo = "Orthic-Labs/rightkit";
const oldSha = "a".repeat(40);
const headSha = "b".repeat(40);
const run = (patch = {}) => ({
  id: 394099, run_number: 810, run_attempt: 2, path: WORKFLOW_PATH,
  repository: { full_name: repo }, head_repository: { full_name: repo }, head_branch: "main", event: "push",
  status: "completed", conclusion: "failure", head_sha: oldSha,
  ...patch,
});
const marker = (patch = {}) => ({
  name: "mark-green-windows", run_id: 394099, run_attempt: 2, head_sha: oldSha, head_branch: "main",
  status: "completed", conclusion: "success", ...patch,
});
const stable = { runner: "1".repeat(64), toolchain: "2".repeat(64), evidence: "3".repeat(64) };
const request = (patch = {}) => ({
  runs: [run()], attempts: [{ runId: 394099, runAttempt: 2, jobs: [marker()] }], repository: repo,
  currentHead: headSha, runner: "windows", ancestryBySha: { [oldSha]: true },
  fingerprintsBySha: { [oldSha]: stable, [headSha]: stable }, ...patch,
});

test("per-runner success proves baseline even when whole workflow failed", () => {
  const result = selectLastGreenCandidate(request());
  assert.equal(result.base, oldSha);
  assert.equal(result.provenance.runAttempt, 2);
  assert.equal(result.provenance.markGreenJob, "mark-green-windows");
  assert.equal(result.provenance.runnerFingerprint, stable.runner);
  assert.equal(result.provenance.toolchainFingerprint, stable.toolchain);
  assert.equal(result.provenance.evidenceContract, EVIDENCE_CONTRACT);
  assert.equal(result.provenance.evidenceFingerprint, stable.evidence);
});

test("missing, failed, skipped, forged, or wrong-attempt marker evidence falls back", () => {
  for (const job of [
    { ...marker(), conclusion: "failure" },
    { ...marker(), conclusion: "skipped", status: "completed" },
    { ...marker(), head_sha: headSha },
    { ...marker(), run_attempt: 1 },
  ]) assert.equal(selectLastGreenCandidate(request({ attempts: [{ runId: 394099, runAttempt: 2, jobs: [job] }] })).base, "");
  assert.equal(selectLastGreenCandidate(request({ attempts: [] })).base, "");
});

test("wrong repository, branch, event, workflow, attempt and cancellation are rejected", () => {
  const mutations = [
    { path: ".github/workflows/publish.yml" },
    { head_repository: { full_name: "fork/rightkit" } },
    { repository: { full_name: "other/rightkit" } },
    { head_branch: "topic" },
    { event: "pull_request" },
    { run_attempt: 1 },
    { conclusion: "cancelled" },
  ];
  for (const patch of mutations) assert.equal(selectLastGreenCandidate(request({ runs: [run(patch)] })).base, "");
});

test("candidate must be verified ancestor and compatibility fingerprints must match", () => {
  assert.equal(selectLastGreenCandidate(request({ ancestryBySha: { [oldSha]: false } })).base, "");
  assert.equal(selectLastGreenCandidate(request({ fingerprintsBySha: { [oldSha]: stable, [headSha]: { ...stable, toolchain: "changed" } } })).base, "");
  assert.equal(selectLastGreenCandidate(request({ fingerprintsBySha: { [oldSha]: stable } })).base, "");
});

test("newest failed marker is bypassed for latest older proven runner marker", () => {
  const older = run({ id: 394098, run_number: 809, run_attempt: 1, head_sha: "c".repeat(40), conclusion: "success" });
  const olderMarker = marker({ run_id: 394098, run_attempt: 1, head_sha: older.head_sha });
  const result = selectLastGreenCandidate(request({
    runs: [run(), older],
    attempts: [
      { runId: 394099, runAttempt: 2, jobs: [marker({ conclusion: "failure" })] },
      { runId: 394098, runAttempt: 1, jobs: [olderMarker] },
    ],
    ancestryBySha: { [oldSha]: true, [older.head_sha]: true },
    fingerprintsBySha: { [oldSha]: stable, [older.head_sha]: stable, [headSha]: stable },
  }));
  assert.equal(result.base, older.head_sha);
});

test("compatibility fingerprints cover declared runner, toolchain, and evidence inputs", () => {
  const files = Object.values(COMPATIBILITY_GROUPS).flat();
  const evidence = Object.fromEntries(files.map((file, index) => [file, { mode: "100644", oid: String(index + 1).padStart(40, "0") }]));
  const original = compatibilityFingerprints(evidence);
  assert.ok(original.runner && original.toolchain && original.evidence);
  assert.notEqual(compatibilityFingerprints({ ...evidence, "scripts/ci/gate.mjs": { mode: "100644", oid: "f".repeat(40) } }).evidence, original.evidence);
  assert.notEqual(compatibilityFingerprints({ ...evidence, "scripts/ci/gate.mjs": { mode: "100755", oid: evidence["scripts/ci/gate.mjs"].oid } }).evidence, original.evidence);
  assert.deepEqual(compatibilityFingerprints({ ...evidence, ".github/workflows/publish.yml": { mode: "100644", oid: "e".repeat(40) } }), original);
  assert.equal(compatibilityFingerprints(Object.fromEntries(Object.entries(evidence).filter(([file]) => file !== "scripts/ci/provision-ort.mjs"))), null);
  assert.notEqual(compatibilityFingerprints({ ...evidence, "scripts/ci/provision-chrome.mjs": { mode: "100644", oid: "d".repeat(40) } }).evidence, original.evidence);
});

test("rust toolchain files are optional, but their presence and contents affect runner fingerprint", () => {
  const required = Object.values(COMPATIBILITY_GROUPS).flat().filter((file) => !["rust-toolchain.toml", "rust-toolchain"].includes(file));
  const base = Object.fromEntries(required.map((file, index) => [file, { mode: "100644", oid: String(index + 1).padStart(40, "0") }]));
  const absent = compatibilityFingerprints(base);
  assert.ok(absent);
  for (const optional of ["rust-toolchain.toml", "rust-toolchain"]) {
    const added = { ...base, [optional]: { mode: "100644", oid: "f".repeat(40) } };
    assert.notEqual(compatibilityFingerprints(added).runner, absent.runner);
    assert.notEqual(compatibilityFingerprints({ ...base, [optional]: { mode: "100644", oid: "e".repeat(40) } }).runner, compatibilityFingerprints(added).runner);
    assert.equal(compatibilityFingerprints(Object.fromEntries(Object.entries(added).filter(([file]) => file !== optional))).runner, absent.runner);
  }
  assert.equal(compatibilityFingerprints(Object.fromEntries(Object.entries(base).filter(([file]) => file !== "scripts/ci/provision-ort.mjs")),), null);
});

test("lockfile changes do not affect compatibility fingerprints", () => {
  const files = Object.values(COMPATIBILITY_GROUPS).flat();
  const evidence = Object.fromEntries(files.map((file, index) => [file, { mode: "100644", oid: String(index + 1).padStart(40, "0") }]));
  const original = compatibilityFingerprints(evidence);
  for (const file of ["Cargo.lock", "pnpm-lock.yaml"]) {
    assert.equal(compatibilityFingerprints({ ...evidence, [file]: { mode: "100644", oid: "f".repeat(40) } }).toolchain, original.toolchain);
  }
});

test("committed-tree inventory fingerprints with actual optional toolchain-file state", () => {
  const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
  const paths = [...new Set([...Object.values(COMPATIBILITY_GROUPS).flat(), "rust-toolchain.toml", "rust-toolchain"] )];
  const result = spawnSync("git", ["ls-tree", "-r", "-z", "--full-tree", "HEAD", "--", ...paths], { cwd: root, encoding: "buffer", windowsHide: true });
  assert.equal(result.status, 0, String(result.stderr ?? ""));
  const inventory = {};
  for (const record of Buffer.from(result.stdout).toString("utf8").split("\0").filter(Boolean)) {
    const match = /^([0-7]{6}) blob ([a-f0-9]{40,64})\t(.+)$/i.exec(record);
    assert.ok(match, `unexpected git tree record: ${record}`);
    inventory[match[3]] = { mode: match[1], oid: match[2] };
  }
  const fingerprints = compatibilityFingerprints(inventory);
  assert.ok(fingerprints, "actual committed tree must fingerprint despite absent optional rust toolchain files");
  for (const optional of ["rust-toolchain.toml", "rust-toolchain"]) {
    assert.equal(optional in inventory, result.stdout.includes(Buffer.from(`\t${optional}\0`)));
  }
});

test("Actions client uses fixed read-only endpoints and exact run attempt pagination", async () => {
  const calls = [];
  const client = createGitHubActionsClient({
    token: "secret-fixture", repository: repo,
    fetchImpl: async (url, options) => {
      calls.push({ url, options });
      if (url.includes("/workflows/")) return { ok: true, json: async () => ({ total_count: 1, workflow_runs: [run()] }) };
      const page = Number(new URL(url).searchParams.get("page"));
      return { ok: true, json: async () => ({ total_count: 101, jobs: page === 1 ? Array.from({ length: 100 }, (_, i) => ({ id: i })) : [{ id: 100 }] }) };
    },
  });
  assert.equal((await client.listRuns())[0].id, 394099);
  assert.equal((await client.listAttemptJobs(394099, 2)).length, 101);
  assert.equal(calls.length, 3);
  assert.ok(calls[0].url.endsWith("/repos/Orthic-Labs/rightkit/actions/workflows/ci.yml/runs?branch=main&event=push&per_page=40&page=1"));
  assert.ok(calls[1].url.endsWith("/repos/Orthic-Labs/rightkit/actions/runs/394099/attempts/2/jobs?per_page=100&page=1"));
  assert.equal(calls[0].options.method, "GET");
  assert.equal(calls[0].options.redirect, "error");
  assert.equal(calls[0].options.headers.Authorization, "Bearer secret-fixture");
  assert.ok(calls.every(({ url }) => !url.includes("secret-fixture")));
});

test("Actions API errors expose no credential and require full baseline", async () => {
  const client = createGitHubActionsClient({ token: "secret-fixture", repository: repo, fetchImpl: async () => ({ ok: false, status: 403 }) });
  await assert.rejects(client.listRuns(), (error) => error.message.includes("full baseline") && !error.message.includes("secret-fixture"));
});

test("resolver derives current commit from Git and writes fail-closed result when token is absent", async () => {
  const required = Object.values(COMPATIBILITY_GROUPS).flat();
  const inventory = Buffer.from(required.map((file, i) => `100644 blob ${String(i + 1).padStart(40, "0")}\t${file}\0`).join(""));
  const gitRunner = (args) => args[0] === "rev-parse"
    ? { status: 0, stdout: `${headSha}\n` }
    : { status: 0, stdout: inventory };
  const result = await resolveLastGreen({ env: {}, fetchImpl: async () => { throw new Error("must not be called"); }, gitRunner });
  assert.equal(result.currentHead, headSha);
  assert.equal(result.results.windows.base, "");
  assert.match(result.results.windows.reason, /GITHUB_TOKEN is unavailable/);
  assert.equal(result.results.macos.base, "");
});

test("resolver proves both runner markers from matching successful attempts", async () => {
  const files = Object.values(COMPATIBILITY_GROUPS).flat();
  const inventory = Buffer.from(files.map((file, i) => `100644 blob ${String(i + 1).padStart(40, "0")}\t${file}\0`).join(""));
  const gitRunner = (args) => {
    if (args[0] === "rev-parse") return { status: 0, stdout: `${headSha}\n` };
    if (args[0] === "merge-base") return { status: 0, stdout: "" };
    if (args[0] === "ls-tree") return { status: 0, stdout: inventory };
    throw new Error(`unexpected git command: ${args[0]}`);
  };
  const calls = [];
  const fetchImpl = async (url) => {
    calls.push(url);
    if (url.includes("/workflows/")) return { ok: true, json: async () => ({ total_count: 1, workflow_runs: [run()] }) };
    const job = url.includes("/attempts/2/jobs")
      ? (calls.filter((item) => item.includes("/attempts/2/jobs")).length === 1 ? marker() : marker({ name: "mark-green-macos" }))
      : null;
    return { ok: true, json: async () => ({ total_count: 1, jobs: [job] }) };
  };
  const result = await resolveLastGreen({
    env: { GITHUB_TOKEN: "secret-fixture", GITHUB_REPOSITORY: repo }, fetchImpl, gitRunner,
  });
  assert.equal(result.currentHead, headSha);
  for (const runner of ["windows", "macos"]) {
    assert.equal(result.results[runner].base, oldSha);
    assert.equal(result.results[runner].provenance.runAttempt, 2);
    assert.equal(result.results[runner].provenance.commit, oldSha);
  }
  assert.equal(calls.filter((url) => url.includes("/attempts/2/jobs")).length, 2);
});

test("resolver rejects changed shared-CI inputs before requesting marker jobs", async () => {
  const files = Object.values(COMPATIBILITY_GROUPS).flat();
  const currentInventory = Buffer.from(files.map((file, i) => `100644 blob ${String(i + 1).padStart(40, "0")}\t${file}\0`).join(""));
  const priorInventory = Buffer.from(files.map((file, i) => `100644 blob ${file === "scripts/ci/select.mjs" ? "f".repeat(40) : String(i + 1).padStart(40, "0")}\t${file}\0`).join(""));
  const gitRunner = (args) => {
    if (args[0] === "rev-parse") return { status: 0, stdout: `${headSha}\n` };
    if (args[0] === "merge-base") return { status: 0, stdout: "" };
    if (args[0] === "ls-tree") return { status: 0, stdout: args[4] === oldSha ? priorInventory : currentInventory };
    throw new Error(`unexpected git command: ${args[0]}`);
  };
  const calls = [];
  const fetchImpl = async (url) => {
    calls.push(url);
    return { ok: true, json: async () => ({ total_count: 1, workflow_runs: [run()] }) };
  };
  const result = await resolveLastGreen({
    env: { GITHUB_TOKEN: "secret-fixture", GITHUB_REPOSITORY: repo }, fetchImpl, gitRunner,
  });
  assert.equal(result.results.windows.base, "");
  assert.equal(result.results.macos.base, "");
  assert.match(result.results.windows.reason, /full baseline/);
  assert.equal(calls.filter((url) => url.includes("/attempts/")).length, 0);
});
