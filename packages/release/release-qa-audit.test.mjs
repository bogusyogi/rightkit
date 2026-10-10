import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { spawnSync } from "./background-process.mjs";
import {
  admitInstalledQa, admitSourceQa, auditMain, auditQa, formatAuditTable, installedQaRecordPath,
} from "./release-qa-audit.mjs";

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

test("source QA refusal warns once, continues, and seals a missing status", () => {
  const root = repo();
  try {
    const lines = [];
    const qa = admitSourceQa(gateOpts(root), { warn: (line) => lines.push(line) });
    assert.equal(qa.status, "missing");
    assert.match(qa.reason, /no source QA receipt for demo-1\.0\.0-abcd1234/);
    assert.equal(lines.length, 1);
    assert.match(lines[0], /^\[right-release\] QA warning: .*; release continues and is recorded as QA missing for audit\. Run: right-release qa --stage source --platform mac/);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("source QA rejected receipt (unavailable key) is recorded as rejected", () => {
  const root = repo();
  try {
    const file = path.join(root, ".right-release", "qa", "demo-1.0.0-abcd1234", "mac", "source.receipt.json");
    mkdirSync(path.dirname(file), { recursive: true });
    writeFileSync(file, JSON.stringify({ schema: 1, stage: "source" }));
    const lines = [];
    const qa = admitSourceQa(gateOpts(root), { warn: (line) => lines.push(line) });
    assert.equal(qa.status, "rejected");
    assert.match(lines[0], /recorded as QA rejected/);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("source QA still throws non-refusal errors", () => {
  assert.throws(() => admitSourceQa({ ...gateOpts(null), repoRoot: undefined }, { warn: () => assert.fail("must not warn") }), TypeError);
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

test("audit: passed, missing source, missing installed, rejected, and not-uploaded rows", () => {
  const root = repo();
  try {
    seal(root, "app-1.0.0-aaaaaaaa", "mac", { status: "passed", sourceReceiptDigest: SHA("1"), floorDigest: SHA("2") });
    installed(root, "app-1.0.0-aaaaaaaa", "mac", { status: "passed", receiptDigest: SHA("3"), checkedAt: "2026-10-06T00:00:00.000Z", provider: "github-releases" });
    seal(root, "app-1.0.1-bbbbbbbb", "mac", { status: "missing", reason: "no source QA receipt" });
    installed(root, "app-1.0.1-bbbbbbbb", "mac", { status: "passed", receiptDigest: SHA("3"), checkedAt: "2026-10-06T00:00:00.000Z", provider: "r2" });
    seal(root, "app-1.0.2-cccccccc", "windows", { status: "passed", sourceReceiptDigest: SHA("1"), floorDigest: SHA("2") });
    installed(root, "app-1.0.2-cccccccc", "windows", { status: "missing", reason: "no installed QA receipt", checkedAt: "2026-10-06T00:00:00.000Z", provider: "r2" });
    seal(root, "app-1.0.3-dddddddd", "mac", { status: "passed", sourceReceiptDigest: SHA("1"), floorDigest: SHA("2") });
    installed(root, "app-1.0.3-dddddddd", "mac", { status: "rejected", reason: "installed QA receipt rejected: stale", checkedAt: "2026-10-06T00:00:00.000Z", provider: "r2" });
    seal(root, "app-1.0.4-eeeeeeee", "mac", { status: "missing", reason: "no source QA receipt" });

    const report = auditQa({ repoRoot: root });
    const byId = Object.fromEntries(report.rows.map((row) => [row.releaseId, row]));
    assert.equal(byId["app-1.0.0-aaaaaaaa"].ok, true);
    assert.equal(byId["app-1.0.1-bbbbbbbb"].source.status, "missing");
    assert.equal(byId["app-1.0.1-bbbbbbbb"].ok, false);
    assert.equal(byId["app-1.0.2-cccccccc"].installed.status, "missing");
    assert.equal(byId["app-1.0.2-cccccccc"].ok, false);
    assert.equal(byId["app-1.0.3-dddddddd"].installed.status, "rejected");
    assert.equal(byId["app-1.0.3-dddddddd"].ok, false);
    assert.equal(byId["app-1.0.4-eeeeeeee"].installed.status, "not-uploaded");
    assert.equal(byId["app-1.0.4-eeeeeeee"].ok, true, "an unuploaded build lacking QA is reported but does not fail the audit");
    assert.equal(report.ok, false);
    const table = formatAuditTable(report);
    assert.match(table, /FLAG/);
    assert.match(table, /3 uploaded build\(s\) lack passing QA/);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("audit exit codes, --json, --since, and legacy manifests", () => {
  const root = repo();
  try {
    const out = [];
    const run = (args) => auditMain(["--app", root, ...args], { stdout: (text) => out.push(text), stderr: (text) => out.push(text) });
    assert.equal(run([]), 0);
    // legacy manifest: sourceReceiptDigest with no status counts as passed; uploaded legacy build without a record is flagged
    seal(root, "old-1.0.0-11111111", "mac", { sourceReceiptDigest: SHA("1"), floorDigest: SHA("2") }, { sealedAt: "2026-09-01T00:00:00.000Z" });
    const state = path.join(root, ".right-release", "state", "old-1.0.0-11111111", "mac");
    mkdirSync(state, { recursive: true });
    writeFileSync(path.join(state, "verified-update-github-releases.json"), "{}");
    seal(root, "new-1.0.0-22222222", "mac", { status: "passed", sourceReceiptDigest: SHA("1"), floorDigest: SHA("2") });
    installed(root, "new-1.0.0-22222222", "mac", { status: "passed", receiptDigest: SHA("3"), checkedAt: "2026-10-06T00:00:00.000Z", provider: "r2" });
    out.length = 0;
    assert.equal(run(["--json"]), 1);
    const json = JSON.parse(out[0]);
    assert.equal(json.rows.find((row) => row.releaseId === "old-1.0.0-11111111").source.status, "passed");
    assert.equal(json.rows.find((row) => row.releaseId === "old-1.0.0-11111111").installed.status, "missing");
    assert.equal(run(["--since", "2026-10-01"]), 0);
    assert.equal(run(["--since", "nope"]), 2);
    assert.equal(run(["--bogus"]), 2);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("right-release qa audit is wired through the CLI", () => {
  const root = repo();
  try {
    seal(root, "app-1.0.0-aaaaaaaa", "mac", { status: "passed", sourceReceiptDigest: SHA("1"), floorDigest: SHA("2") });
    const cli = path.join(path.dirname(new URL(import.meta.url).pathname), "cli", "right-release.mjs");
    const result = spawnSync(process.execPath, [cli, "qa", "audit", "--app", root], { encoding: "utf8" });
    assert.equal(result.status, 0, result.stderr);
    assert.match(result.stdout, /app-1\.0\.0-aaaaaaaa/);
    assert.match(result.stdout, /all uploaded builds have passing QA/);
  } finally { rmSync(root, { recursive: true, force: true }); }
});
