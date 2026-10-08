import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { buildOwnershipReport, publishedAvailability, renderOwnershipMarkdown } from "./ownership-report.mjs";

const manifest = { schema: 1, capabilities: [
  { package: "rightkit-oauth", capability: "oauth-sign-in", summary: "OAuth", fingerprints: [] },
  { package: "rightkit-new", capability: "new-capability", summary: "New", fingerprints: [] },
] };
const versions = { cargo: {}, stagedCargo: { "rightkit-oauth": "0.2.1" }, npm: {}, stagedNpm: {} };

test("availability labels unverified staging separately from publication", () => {
  assert.deepEqual(publishedAvailability(manifest.capabilities[0], versions), [{ registry: "crates.io", package: "rightkit-oauth", version: "0.2.1", state: "staged-unverified" }]);
  assert.deepEqual(publishedAvailability(manifest.capabilities[1], versions), []);
});

test("report keeps advisory findings out of blocks & declarations out of verified integration", () => {
  const workspaceRoot = mkdtempSync(path.join(tmpdir(), "rk-report-tiers-"));
  mkdirSync(path.join(workspaceRoot, "app"));
  const policy = { capabilities: [
    { package: "rightkit-test", capability: "test-capability", detection: "fingerprinted", fingerprints: [{ id: "test.npm.library", tier: "advise" }] },
    { package: "rightkit-declared", capability: "declared-capability", detection: "declaration-only", fingerprints: [] },
  ] };
  const finding = { fingerprint: "test.npm.library", capability: "test-capability", file: "package.json" };
  try {
    const report = buildOwnershipReport({ workspaceRoot, manifest: policy, versions: {}, apps: [{ key: "app", root: "app" }], scanner: () => ({
      rows: [{ capability: "test-capability", status: "blocked" }, { capability: "declared-capability", status: "verified-integration", uses: true }],
      findings: [finding], violations: [finding], covered: [], problems: [], coverage: [],
    }) });
    assert.deepEqual(report.blocks, []);
    assert.equal(report.apps[0].advisories.length, 1);
    assert.equal(report.apps[0].rows[0].status, "advisory");
    assert.equal(report.apps[0].rows[1].status, "declared-dependency");
    assert.match(renderOwnershipMarkdown(report), /declaration-only/);
  } finally { rmSync(workspaceRoot, { recursive: true, force: true }); }
});

test("cross-app report keeps dependency distinct from integration and hashes/deduplicates scan evidence", () => {
  const workspaceRoot = mkdtempSync(path.join(tmpdir(), "rk-report-"));
  for (const name of ["app-a", "app-b"]) mkdirSync(path.join(workspaceRoot, name));
  const scanner = (root) => ({
    ok: true,
    rows: [
      { capability: "oauth-sign-in", owner: "rightkit-oauth", uses: true, status: "declared-dependency", count: 0 },
      { capability: "new-capability", owner: "rightkit-new", uses: false, status: "not-detected", count: 0 },
    ],
    findings: [],
    violations: [{ fingerprint: "oauth.endpoint.github", file: "src/auth.rs", occurrenceHash: "same-block-digest" }],
    covered: [], problems: [], coverage: [],
    metadata: { repositoryIdentity: root, commit: "abc123", host: "host/windows", scannedAt: "2026-10-07T00:00:00.000Z", catalogDigest: "catalog" },
  });
  try {
    const report = buildOwnershipReport({ workspaceRoot, manifest, versions, apps: [{ key: "a", root: "app-a" }, { key: "b", root: "app-b" }], now: new Date("2026-10-07T00:00:10.000Z"), scanner });
    assert.equal(report.blocks.length, 1);
    assert.equal(report.blocks[0].contentHash, "same-block-digest");
    assert.deepEqual(report.blocks[0].apps, ["a", "b"]);
    assert.equal(report.apps[0].rows[0].status, "declared-dependency");
    assert.equal(report.apps[0].rows[1].status, "not-detected");
    assert.equal(report.apps[0].scanAgeSeconds, 10);
    assert.deepEqual(report.apps[0].contentDigests, []);
    assert.match(renderOwnershipMarkdown(report), /declared-dependency/);
    assert.match(renderOwnershipMarkdown(report), /staged-unverified/);
  } finally { rmSync(workspaceRoot, { recursive: true, force: true }); }
});

test("raw exception records become pending requests and are never treated as approved", () => {
  const workspaceRoot = mkdtempSync(path.join(tmpdir(), "rk-exception-feed-"));
  const appRoot = path.join(workspaceRoot, "app"); mkdirSync(path.join(appRoot, ".rightkit"), { recursive: true });
  writeFileSync(path.join(appRoot, ".rightkit", "ownership-exceptions.json"), JSON.stringify({ exceptions: [{ requestId: "RK-ABCDEF", fingerprint: "oauth.endpoint.github", paths: ["src/auth.rs"], reason: "migration", owner: "app" }] }));
  try {
    const report = buildOwnershipReport({ workspaceRoot, manifest, versions, apps: [{ key: "app", root: "app" }], scanner: () => ({ enrollmentPending: true, rows: [], findings: [], violations: [], covered: [], problems: [], coverage: [], metadata: {} }) });
    assert.equal(report.exceptionRequests.length, 1);
    assert.equal(report.exceptionRequests[0].state, "request-pending-verification");
    assert.deepEqual(report.apps[0].covered, []);
  } finally { rmSync(workspaceRoot, { recursive: true, force: true }); }
});

test("pending enrollment report preserves findings as warnings without creating blocks", () => {
  const workspaceRoot = mkdtempSync(path.join(tmpdir(), "rk-pending-report-"));
  mkdirSync(path.join(workspaceRoot, "app"));
  try {
    const report = buildOwnershipReport({
      workspaceRoot,
      manifest,
      versions,
      apps: [{ key: "app", root: "app" }],
      scanner: () => ({
        ok: true,
        enrollmentPending: true,
        warnings: ["approval enrollment pending; findings remain backlog requests"],
        rows: [{ capability: "oauth-sign-in", owner: "rightkit-oauth", uses: false, status: "blocked", count: 1 }],
        findings: [{ fingerprint: "oauth.endpoint.github", file: "src/auth.rs" }],
        violations: [{ fingerprint: "oauth.endpoint.github", file: "src/auth.rs", occurrenceHash: "pending-finding" }],
        covered: [], problems: [], coverage: [], metadata: { catalogDigest: "catalog" },
      }),
    });
    assert.equal(report.apps[0].status, "warning");
    assert.equal(report.apps[0].enrollmentPending, true);
    assert.equal(report.apps[0].rows[0].status, "enrollment-pending");
    assert.equal(report.blocks.length, 0);
    assert.equal(report.warnings[0].findings[0].fingerprint, "oauth.endpoint.github");
    assert.match(renderOwnershipMarkdown(report), /WARNING: approval enrollment pending/);
  } finally { rmSync(workspaceRoot, { recursive: true, force: true }); }
});
