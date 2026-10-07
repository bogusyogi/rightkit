import assert from "node:assert/strict";
import { generateKeyPairSync } from "node:crypto";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { spawnSync } from "node:child_process";
import { publishPublicOwnershipCheckForTest, runProtectedScanForTest, __testOnly } from "./public-check-producer.mjs";

function git(root, ...args) {
  const result = spawnSync("git", ["-C", root, ...args], { encoding: "utf8", windowsHide: true });
  assert.equal(result.status, 0, result.stderr);
  return result.stdout.trim();
}
function fixture() {
  const root = mkdtempSync(path.join(tmpdir(), "rightkit-public-check-"));
  git(root, "init", "-q");
  git(root, "config", "user.email", "test@example.invalid");
  git(root, "config", "user.name", "test");
  git(root, "remote", "add", "origin", "https://github.com/test/public-repo.git");
  writeFileSync(path.join(root, "source.txt"), "public source\n");
  git(root, "add", ".");
  git(root, "commit", "-qm", "baseline");
  return root;
}

test("public producer binds external App check to protected public registration and exact commit", async () => {
  const root = fixture();
  const outside = mkdtempSync(path.join(tmpdir(), "rightkit-public-app-config-"));
  const config = path.join(outside, "public-check.json");
  const keyPath = path.join(outside, "app.pem");
  const keys = generateKeyPairSync("rsa", { modulusLength: 2048 });
  writeFileSync(config, JSON.stringify({ schema: 1, appId: 9912, installationId: 67123, checkName: __testOnly.CHECK_NAME, service: { windowsSid: "S-1-5-21-1-2-3-4001", agentWindowsSids: ["S-1-5-21-1-2-3-4002"], windowsCheckoutRoot: "D:\\runner", macosUid: 600, agentMacosUids: [501], macosCheckoutRoot: "/var/lib/rightkit/checkouts" } }));
  writeFileSync(keyPath, keys.privateKey.export({ type: "pkcs8", format: "pem" }));
  const commit = git(root, "rev-parse", "HEAD");
  const tree = git(root, "rev-parse", "HEAD^{tree}");
  const trustModule = {
    loadTrustedExternal: () => ({ trust: { repositories: [] } }),
    verifyRepositoryRegistration: (_trust, identity) => ({ ok: true, registration: { visibility: "public", roots: ["."] }, identity }),
  };
  const scanner = async () => ({ ok: true, evidence: { commit, tree, repositoryIdentity: "https://github.com/test/public-repo", visibility: "public", catalogDigest: "a".repeat(64), registeredRoots: ["."] } });
  const calls = [];
  const apiCall = async (url, token, options = {}) => {
    calls.push({ url, token, options });
    if (url.endsWith("/access_tokens")) return { token: "ephemeral-test-token" };
    if (options.method === "POST") return { id: 501, app: { id: 9912 }, head_sha: commit, name: __testOnly.CHECK_NAME, status: "completed", conclusion: "success" };
    return { check_runs: [{ id: 501, app: { id: 9912 }, head_sha: commit, name: __testOnly.CHECK_NAME, status: "completed", conclusion: "success" }] };
  };
  try {
    const result = await publishPublicOwnershipCheckForTest({ repoRoot: root, expectedCommit: commit, platform: "win32", audit: () => ({ ok: true }), configPath: config, keyPath, trustModule, scanner, apiCall });
    assert.equal(result.ok, true);
    assert.equal(result.appId, 9912);
    assert.equal(result.commit, commit);
    assert.equal(calls.length, 3);
    assert.match(calls[1].url, /\/repos\/test\/public-repo\/check-runs$/);
    assert.equal(JSON.parse(calls[1].options.body).head_sha, commit);
    assert.equal(calls[1].token, "ephemeral-test-token");

    const wrongApp = async (url, token, options = {}) => {
      if (url.endsWith("/access_tokens")) return { token: "ephemeral-test-token" };
      if (options.method === "POST") return { id: 502, app: { id: 2 }, head_sha: commit, name: __testOnly.CHECK_NAME, status: "completed", conclusion: "success" };
      return { check_runs: [] };
    };
    await assert.rejects(publishPublicOwnershipCheckForTest({ repoRoot: root, expectedCommit: commit, platform: "win32", audit: () => ({ ok: true }), configPath: config, keyPath, trustModule, scanner, apiCall: wrongApp }), /without expected App authority/);
    await assert.rejects(publishPublicOwnershipCheckForTest({ repoRoot: root, expectedCommit: "0".repeat(commit.length), platform: "win32", audit: () => ({ ok: true }), configPath: config, keyPath, trustModule, scanner, apiCall }), /does not match externally requested/);
  } finally { rmSync(root, { recursive: true, force: true }); rmSync(outside, { recursive: true, force: true }); }
});

test("public App config rejects repository-selected or unexpected check authority", () => {
  const service = { windowsSid: "S-1-5-21-1-2-3-4001", agentWindowsSids: ["S-1-5-21-1-2-3-4002"], windowsCheckoutRoot: "D:\\runner", macosUid: 600, agentMacosUids: [501], macosCheckoutRoot: "/var/lib/rightkit/checkouts" };
  assert.throws(() => __testOnly.validateAppConfig({ schema: 1, appId: 1, installationId: 2, checkName: "Attacker check", service }), /malformed/);
  assert.throws(() => __testOnly.validateAppConfig({ schema: 1, appId: "1", installationId: 2, checkName: __testOnly.CHECK_NAME, service }), /malformed/);
});

test("public scanner timeout, process failure, and malformed output have typed failure classes", () => {
  const timeout = () => ({ status: null, error: { code: "ETIMEDOUT" } });
  assert.throws(() => runProtectedScanForTest("D:\\runner\\repo", "a".repeat(40), timeout), (error) => error.type === "protected-scan-timeout");
  const failed = () => ({ status: 1, stdout: JSON.stringify({ ok: false, error: { type: "protected-registration-rejected", message: "not registered" } }) });
  assert.throws(() => runProtectedScanForTest("D:\\runner\\repo", "a".repeat(40), failed), (error) => error.type === "protected-registration-rejected");
  const malformed = () => ({ status: 0, stdout: "{}" });
  assert.throws(() => runProtectedScanForTest("D:\\runner\\repo", "a".repeat(40), malformed), (error) => error.type === "protected-scan-output-invalid");
  const accepted = (_exe, _args, options) => {
    assert.equal(options.timeout, 120000);
    assert.equal(options.env.GIT_NO_REPLACE_OBJECTS, "1");
    assert.equal(options.env.NODE_OPTIONS, undefined);
    return { status: 0, stdout: JSON.stringify({ ok: true, evidence: {} }) };
  };
  assert.equal(runProtectedScanForTest("D:\\runner\\repo", "a".repeat(40), accepted).ok, true);
});
