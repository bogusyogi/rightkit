import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { spawnSync } from "./background-process.mjs";
import {
  admitInstalledQa, auditMain, auditQa, formatAuditTable, installedQaRecordPath,
} from "./release-qa-audit.mjs";
import { fileURLToPath } from "node:url";
import { policyWith } from "./release-qa-fixtures.mjs";
import { QaAdmissionError } from "./release-qa-receipt.mjs";

const SHA = (c) => c.repeat(64);
const refusingKey = { read() { throw new Error("no key in test"); } };

function repo() {
  const root = mkdtempSync(path.join(tmpdir(), "rk-qa-audit-"));
  return root;
}

function seal(root, releaseId, platformDir, qa, extra = {}) {
  const dir = path.join(root, ".right-release", "sealed", releaseId, platformDir);
  mkdirSync(dir, { recursive: true });
  writeFileSync(path.join(dir, "release-manifest.json"), JSON.stringify({ schema: 1, releaseId, version: "1.0.0", qa, sealedAt: "2026-10-05T00:00:00.000Z", ...extra }));
}
function installed(root, releaseId, platformDir, record) {
  const dir = path.join(root, ".right-release", "state", releaseId, platformDir);
  mkdirSync(dir, { recursive: true });
  writeFileSync(path.join(dir, "qa-installed.json"), JSON.stringify(record));
}

const gateOpts = (root) => ({
  repoRoot: root, releaseId: "demo-1.0.0-abcd1234", platform: "mac", configName: "right-release.config.mjs", releaseConfigSha256: SHA("d"),
  credentialStore: refusingKey,
});

test("installed QA still throws non-refusal errors", () => {
  assert.throws(() => admitInstalledQa({ ...gateOpts(null), repoRoot: undefined, manifest: { releaseId: "demo-1.0.0-abcd1234", commit: "x", files: [] } }, { provider: "r2", warn: () => assert.fail("must not warn") }), TypeError);
});

test("installed QA refusal warns, records qa-installed.json, and does not throw", () => {
  const root = repo();
  try {
    const lines = [];
    const manifest = { releaseId: "demo-1.0.0-abcd1234", commit: "x", files: [] };
    const record = admitInstalledQa({ repoRoot: root, manifest, platform: "mac", configName: "right-release.config.mjs", releaseConfigSha256: SHA("d"), credentialStore: refusingKey },
      { provider: "r2", warn: (line) => lines.push(line), now: () => new Date("2026-10-10T01:02:03.000Z") });
    assert.equal(record.status, "missing");
    assert.equal(lines.length, 1);
    assert.match(lines[0], /QA warning: no installed QA receipt.*recorded as QA missing for audit\. Run: right-release qa --stage installed --platform mac --release demo-1.0.0-abcd1234/);
    const file = installedQaRecordPath({ repoRoot: root, releaseId: manifest.releaseId, platform: "mac" });
    assert.ok(file.endsWith(path.join("state", "demo-1.0.0-abcd1234", "mac", "qa-installed.json")));
    assert.deepEqual(JSON.parse(readFileSync(file, "utf8")), { status: "missing", reason: record.reason, checkedAt: "2026-10-10T01:02:03.000Z", provider: "r2" });
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("audit: passed, missing installed, rejected, and not-uploaded rows", () => {
  const root = repo();
  try {
    seal(root, "app-1.0.0-aaaaaaaa", "mac", { floorDigest: SHA("2") });
    installed(root, "app-1.0.0-aaaaaaaa", "mac", { status: "passed", receiptDigest: SHA("3"), checkedAt: "2026-10-06T00:00:00.000Z", provider: "github-releases" });
    seal(root, "app-1.0.2-cccccccc", "windows", { floorDigest: SHA("2") });
    installed(root, "app-1.0.2-cccccccc", "windows", { status: "missing", reason: "no installed QA receipt", checkedAt: "2026-10-06T00:00:00.000Z", provider: "r2" });
    seal(root, "app-1.0.3-dddddddd", "mac", { floorDigest: SHA("2") });
    installed(root, "app-1.0.3-dddddddd", "mac", { status: "rejected", reason: "installed QA receipt rejected: stale", checkedAt: "2026-10-06T00:00:00.000Z", provider: "r2" });
    seal(root, "app-1.0.4-eeeeeeee", "mac", {});

    const report = auditQa({ repoRoot: root });
    const byId = Object.fromEntries(report.rows.map((row) => [row.releaseId, row]));
    assert.equal(byId["app-1.0.0-aaaaaaaa"].ok, true);
    assert.equal(byId["app-1.0.2-cccccccc"].installed.status, "missing");
    assert.equal(byId["app-1.0.2-cccccccc"].ok, false);
    assert.equal(byId["app-1.0.3-dddddddd"].installed.status, "rejected");
    assert.equal(byId["app-1.0.3-dddddddd"].ok, false);
    assert.equal(byId["app-1.0.4-eeeeeeee"].installed.status, "not-uploaded");
    assert.equal(byId["app-1.0.4-eeeeeeee"].ok, true, "an unuploaded build lacking QA is reported but does not fail the audit");
    assert.equal(report.ok, false);
    const table = formatAuditTable(report);
    assert.match(table, /FLAG/);
    assert.match(table, /2 uploaded build\(s\) lack passing QA/);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("audit exit codes, --json, --since, and legacy manifests", () => {
  const root = repo();
  try {
    const out = [];
    const run = (args) => auditMain(["--app", root, ...args], { stdout: (text) => out.push(text), stderr: (text) => out.push(text) });
    assert.equal(run([]), 0);
    // a legacy manifest (it still records a `qa` block, no longer read) with an uploaded build and no installed record is flagged
    seal(root, "old-1.0.0-11111111", "mac", { floorDigest: SHA("2") }, { sealedAt: "2026-09-01T00:00:00.000Z" });
    const state = path.join(root, ".right-release", "state", "old-1.0.0-11111111", "mac");
    mkdirSync(state, { recursive: true });
    writeFileSync(path.join(state, "verified-update-github-releases.json"), "{}");
    seal(root, "new-1.0.0-22222222", "mac", { floorDigest: SHA("2") });
    installed(root, "new-1.0.0-22222222", "mac", { status: "passed", receiptDigest: SHA("3"), checkedAt: "2026-10-06T00:00:00.000Z", provider: "r2" });
    out.length = 0;
    assert.equal(run(["--json"]), 1);
    const json = JSON.parse(out[0]);
    assert.equal(json.rows.find((row) => row.releaseId === "old-1.0.0-11111111").installed.status, "missing");
    assert.equal(run(["--since", "2026-10-01"]), 0);
    assert.equal(run(["--since", "nope"]), 2);
    assert.equal(run(["--bogus"]), 2);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("right-release qa audit is wired through the CLI", () => {
  const root = repo();
  try {
    seal(root, "app-1.0.0-aaaaaaaa", "mac", { floorDigest: SHA("2") });
    const cli = path.join(path.dirname(fileURLToPath(import.meta.url)), "cli", "right-release.mjs");
    const result = spawnSync(process.execPath, [cli, "qa", "audit", "--app", root], { encoding: "utf8" });
    assert.equal(result.status, 0, result.stderr);
    assert.match(result.stdout, /app-1\.0\.0-aaaaaaaa/);
    assert.match(result.stdout, /all uploaded builds have passing QA/);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

// C4-22 (GATE-CONTRACT GC-A07: installed is the only admission): the installed stage reads its own entry, else the app-level mode.
const stagedPolicy = (stages) => policyWith({ apps: { demo: { stages } } });
const refuseEntry = { mode: "refuse", since: "2026-10-24", set_by: "adrian" };
const warnEntry = { mode: "warn", since: "2026-10-24", set_by: "adrian" };
const installedOpts = (root) => ({ repoRoot: root, manifest: { releaseId: "demo-1.0.0-abcd1234", commit: "x", files: [] }, platform: "mac", configName: "right-release.config.mjs", releaseConfigSha256: SHA("d"), credentialStore: refusingKey });

test("C4-22: installed refuse refuses publication; an app-level refuse governs it and a stage entry relaxes it", () => {
  const root = repo();
  try {
    const policy = stagedPolicy({ installed: refuseEntry });
    assert.throws(() => admitInstalledQa(installedOpts(root), { provider: "r2", warn: () => assert.fail("must not warn"), policy }),
      (error) => error instanceof QaAdmissionError && /QA admission refused for demo \(policy refuse since 2026-10-24 by adrian\): no installed QA receipt/.test(error.message));
    const both = policyWith({ apps: { demo: refuseEntry } });
    assert.throws(() => admitInstalledQa(installedOpts(root), { provider: "r2", warn: () => assert.fail("must not warn"), policy: both }), QaAdmissionError);
    const relaxed = policyWith({ apps: { demo: { ...refuseEntry, stages: { installed: warnEntry } } } });
    const lines = [];
    const record = admitInstalledQa(installedOpts(root), { provider: "r2", warn: (line) => lines.push(line), policy: relaxed, now: () => new Date("2026-10-10T01:02:03.000Z") });
    assert.equal(record.status, "missing");
    assert.equal(lines.length, 1);
    assert.match(lines[0], /QA warning: no installed QA receipt.*recorded as QA missing for audit/);
    const file = installedQaRecordPath({ repoRoot: root, releaseId: "demo-1.0.0-abcd1234", platform: "mac" });
    assert.deepEqual(JSON.parse(readFileSync(file, "utf8")), { status: "missing", reason: record.reason, checkedAt: "2026-10-10T01:02:03.000Z", provider: "r2" });
  } finally { rmSync(root, { recursive: true, force: true }); }
});
