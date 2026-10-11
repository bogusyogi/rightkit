import assert from "node:assert/strict";
import { createHash, generateKeyPairSync, sign as cryptoSign } from "node:crypto";
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { spawnSync } from "node:child_process";
import { publishPublicOwnershipCheckForTest, runProtectedScanForTest, __testOnly } from "./public-check-producer.mjs";
import { createKeyEvent, eventHash, keyFingerprint } from "../../packages/release/ownership-key-log.mjs";
import { trustedPath } from "../../packages/release/ownership-approval.mjs";

// The producer reads the protected ownership trust from its fixed host path (trustedPath) for the key authority. A CI
// runner has none, so this test skips with that reason; RIGHTKIT_REQUIRE_ASSETS=1 runs it so the missing trust fails it.
const protectedTrustSkip = (() => {
  let present = false;
  try { present = existsSync(trustedPath()); } catch { present = false; }
  return present || process.env.RIGHTKIT_REQUIRE_ASSETS === "1" ? false : "protected ownership trust is not installed on this host";
})();

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
const canonical = (value) => value === null || typeof value !== "object" ? JSON.stringify(value)
  : Array.isArray(value) ? `[${value.map(canonical).join(",")}]`
    : `{${Object.keys(value).sort().map((key) => `${JSON.stringify(key)}:${canonical(value[key])}`).join(",")}}`;
function signed(draft, pair) { return { ...draft, signature: { keyId: pair.record.keyId, host: pair.record.platform, value: cryptoSign("sha256", Buffer.from(canonical(draft.payload)), pair.privateKey).toString("base64") } }; }
function keyPair() {
  const pair = generateKeyPairSync("ec", { namedCurve: "prime256v1" });
  const publicKeyPem = pair.publicKey.export({ type: "spki", format: "pem" });
  return { privateKey: pair.privateKey, record: { keyId: keyFingerprint(publicKeyPem), publicKeyPem, platform: "windows", deviceName: "test root" } };
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

test("public producer advances protected key-log state outside immutable policy config", async () => {
  const root = fixture();
  const outside = mkdtempSync(path.join(tmpdir(), "rightkit-public-keylog-"));
  const configPath = path.join(outside, "public-check.json");
  const keyPath = path.join(outside, "app.pem");
  const checkpointDirectory = path.join(outside, "checkpoints");
  const fs = await import("node:fs");
  fs.mkdirSync(checkpointDirectory);
  const key = keyPair();
  const genesis = { schema: 1, events: [signed(createKeyEvent({ schema: 1, events: [] }, { operation: "root", subject: key.record, signerKeyId: key.record.keyId }), key)] };
  const other = keyPair();
  const draft = createKeyEvent(genesis, { operation: "add", subject: other.record, signerKeyId: key.record.keyId });
  const log = { schema: 1, events: [...genesis.events, signed(draft, key)] };
  const checkpoint = (value) => ({ sequence: value.events.length - 1, head: eventHash(value.events.at(-1)) });
  const oldCheckpoint = checkpoint(genesis);
  const currentCheckpoint = checkpoint(log);
  const dir = path.join(root, ".rightkit");
  fs.mkdirSync(dir);
  writeFileSync(path.join(dir, "ownership-keys.json"), JSON.stringify(log));
  git(root, "add", ".rightkit/ownership-keys.json");
  git(root, "commit", "-qm", "signed key log");
  const config = { schema: 1, appId: 9912, installationId: 67123, checkName: __testOnly.CHECK_NAME, rootFingerprint: key.record.keyId, keyCheckpoint: oldCheckpoint,
    service: { windowsSid: "S-1-5-21-1-2-3-4001", agentWindowsSids: ["S-1-5-21-1-2-3-4002"], windowsCheckoutRoot: "D:\\runner", macosUid: 600, agentMacosUids: [501], macosCheckoutRoot: "/var/lib/rightkit/checkouts" } };
  writeFileSync(configPath, JSON.stringify(config));
  const keys = generateKeyPairSync("rsa", { modulusLength: 2048 });
  writeFileSync(keyPath, keys.privateKey.export({ type: "pkcs8", format: "pem" }));
  const commit = git(root, "rev-parse", "HEAD");
  const tree = git(root, "rev-parse", "HEAD^{tree}");
  const digest = createHash("sha256").update(readFileSync(path.join(dir, "ownership-keys.json"))).digest("hex");
  const trustModule = { loadTrustedExternal: () => ({ trust: { repositories: [] } }), verifyRepositoryRegistration: () => ({ ok: true, registration: { visibility: "public", roots: ["."] } }) };
  const scanner = async () => ({ ok: true, evidence: { commit, tree, repositoryIdentity: "https://github.com/test/public-repo", visibility: "public", catalogDigest: "a".repeat(64), registeredRoots: ["."], keyLogDigest: digest, rootFingerprint: key.record.keyId, keyCheckpoint: currentCheckpoint } });
  let conclusion = "success";
  const apiCall = async (url, _token, options = {}) => url.endsWith("/access_tokens") ? { token: "test-token" }
    : options.method === "POST" ? (conclusion = JSON.parse(options.body).conclusion, { id: 1, app: { id: 9912 }, head_sha: commit, name: __testOnly.CHECK_NAME, status: "completed", conclusion })
      : { check_runs: [{ id: 1, app: { id: 9912 }, head_sha: commit, name: __testOnly.CHECK_NAME, status: "completed", conclusion }] };
  try {
    const result = await publishPublicOwnershipCheckForTest({ repoRoot: root, expectedCommit: commit, platform: "win32", audit: () => ({ ok: true }), configPath, keyPath, trustModule, scanner, apiCall });
    assert.equal(result.ok, true);
    const configAfter = JSON.parse(readFileSync(configPath, "utf8"));
    assert.deepEqual(configAfter, config);
    const statePath = path.join(checkpointDirectory, `${key.record.keyId}.json`);
    const stateAfter = JSON.parse(readFileSync(statePath, "utf8"));
    assert.equal(stateAfter.rootFingerprint, key.record.keyId);
    assert.deepEqual(stateAfter.keyCheckpoint, currentCheckpoint);
    assert.equal(fs.existsSync(`${statePath}.lock`), false);

    const repeated = await publishPublicOwnershipCheckForTest({ repoRoot: root, expectedCommit: commit, platform: "win32", audit: () => ({ ok: true }), configPath, keyPath, trustModule, scanner, apiCall });
    assert.equal(repeated.ok, true);
    assert.deepEqual(JSON.parse(readFileSync(statePath, "utf8")), stateAfter);

    const lockPath = path.join(checkpointDirectory, `${key.record.keyId}.lock`);
    writeFileSync(lockPath, `${process.pid}\n`);
    await assert.rejects(publishPublicOwnershipCheckForTest({ repoRoot: root, expectedCommit: commit, platform: "win32", audit: () => ({ ok: true }), configPath, keyPath, trustModule, scanner, apiCall }), /lock already exists/);
    assert.deepEqual(JSON.parse(readFileSync(statePath, "utf8")), stateAfter);
    rmSync(lockPath);

    const forkedKey = keyPair();
    const forkEvent = signed(createKeyEvent(genesis, { operation: "add", subject: forkedKey.record, signerKeyId: key.record.keyId }), key);
    const forked = { ...stateAfter, keyCheckpoint: { sequence: 1, head: eventHash(forkEvent) } };
    writeFileSync(statePath, JSON.stringify(forked));
    await assert.rejects(publishPublicOwnershipCheckForTest({ repoRoot: root, expectedCommit: commit, platform: "win32", audit: () => ({ ok: true }), configPath, keyPath, trustModule, scanner, apiCall }), /forks before or at protected checkpoint/);
    assert.deepEqual(JSON.parse(readFileSync(statePath, "utf8")), forked);
    assert.deepEqual(JSON.parse(readFileSync(configPath, "utf8")), config);

    const stale = { ...stateAfter, keyCheckpoint: { sequence: 2, head: "f".repeat(64) } };
    writeFileSync(statePath, JSON.stringify(stale));
    await assert.rejects(publishPublicOwnershipCheckForTest({ repoRoot: root, expectedCommit: commit, platform: "win32", audit: () => ({ ok: true }), configPath, keyPath, trustModule, scanner, apiCall }), /key log is shorter than protected checkpoint/);
    assert.deepEqual(JSON.parse(readFileSync(statePath, "utf8")), stale);
    assert.deepEqual(JSON.parse(readFileSync(configPath, "utf8")), config);
  } finally { rmSync(root, { recursive: true, force: true }); rmSync(outside, { recursive: true, force: true }); }
});

test("public producer uses protected shared authority when product checkout has no key-log copy", { skip: protectedTrustSkip }, async () => {
  const product = fixture();
  const authority = mkdtempSync(path.join(tmpdir(), "rightkit-key-authority-"));
  const outside = mkdtempSync(path.join(tmpdir(), "rightkit-shared-authority-app-"));
  const configPath = path.join(outside, "public-check.json");
  const keyPath = path.join(outside, "app.pem");
  const checkpoints = path.join(outside, "checkpoints");
  const fs = await import("node:fs");
  fs.mkdirSync(checkpoints);
  git(authority, "init", "-q");
  git(authority, "config", "user.email", "test@example.invalid");
  git(authority, "config", "user.name", "test");
  git(authority, "remote", "add", "origin", "https://github.com/test/authority.git");
  const key = keyPair();
  const genesis = { schema: 1, events: [signed(createKeyEvent({ schema: 1, events: [] }, { operation: "root", subject: key.record, signerKeyId: key.record.keyId }), key)] };
  const delegate = keyPair();
  const log = { schema: 1, events: [...genesis.events, signed(createKeyEvent(genesis, { operation: "add", subject: delegate.record, signerKeyId: key.record.keyId }), key)] };
  const logBytes = Buffer.from(JSON.stringify(log));
  const logDirectory = path.join(authority, ".rightkit");
  fs.mkdirSync(logDirectory);
  writeFileSync(path.join(logDirectory, "ownership-keys.json"), logBytes);
  git(authority, "add", ".");
  git(authority, "commit", "-qm", "shared key authority");
  const authorityCommit = git(authority, "rev-parse", "HEAD");
  const authorityTree = git(authority, "rev-parse", "HEAD^{tree}");
  const initialCheckpoint = { sequence: 0, head: eventHash(genesis.events[0]) };
  const keyCheckpoint = { sequence: 1, head: eventHash(log.events[1]) };
  const keyAuthority = { repositoryIdentity: "https://github.com/test/authority", checkoutPath: authority };
  const config = { schema: 1, appId: 9912, installationId: 67123, checkName: __testOnly.CHECK_NAME, rootFingerprint: key.record.keyId, keyCheckpoint: initialCheckpoint, keyAuthority,
    service: { windowsSid: "S-1-5-21-1-2-3-4001", agentWindowsSids: ["S-1-5-21-1-2-3-4002"], windowsCheckoutRoot: "D:\\runner", macosUid: 600, agentMacosUids: [501], macosCheckoutRoot: "/var/lib/rightkit/checkouts" } };
  writeFileSync(configPath, JSON.stringify(config));
  const keys = generateKeyPairSync("rsa", { modulusLength: 2048 });
  writeFileSync(keyPath, keys.privateKey.export({ type: "pkcs8", format: "pem" }));
  const commit = git(product, "rev-parse", "HEAD");
  const tree = git(product, "rev-parse", "HEAD^{tree}");
  const trustModule = { loadTrustedExternal: () => ({ trust: { repositories: [] } }), verifyRepositoryRegistration: () => ({ ok: true, registration: { visibility: "public", roots: ["."] } }) };
  const evidence = { commit, tree, repositoryIdentity: "https://github.com/test/public-repo", visibility: "public", catalogDigest: "a".repeat(64), registeredRoots: ["."],
    keyLogDigest: createHash("sha256").update(logBytes).digest("hex"), rootFingerprint: key.record.keyId, keyCheckpoint,
    keyAuthority: { repositoryIdentity: keyAuthority.repositoryIdentity, commit: authorityCommit, tree: authorityTree } };
  const scanner = async () => ({ ok: true, evidence });
  let conclusion = "success";
  const apiCall = async (url, _token, options = {}) => url.endsWith("/access_tokens") ? { token: "test-token" }
    : options.method === "POST" ? (conclusion = JSON.parse(options.body).conclusion, { id: 1, app: { id: 9912 }, head_sha: commit, name: __testOnly.CHECK_NAME, status: "completed", conclusion })
      : { check_runs: [{ id: 1, app: { id: 9912 }, head_sha: commit, name: __testOnly.CHECK_NAME, status: "completed", conclusion }] };
  try {
    assert.equal(git(product, "ls-tree", "-r", "--name-only", "HEAD", "--", ".rightkit/ownership-keys.json"), "");
    const result = await publishPublicOwnershipCheckForTest({ repoRoot: product, expectedCommit: commit, platform: "win32", audit: () => ({ ok: true }), configPath, keyPath, trustModule, scanner, apiCall });
    assert.equal(result.ok, true);
    assert.deepEqual(JSON.parse(readFileSync(path.join(checkpoints, `${key.record.keyId}.json`), "utf8")).keyCheckpoint, keyCheckpoint);
    const wrongScanner = async () => ({ ok: true, evidence: { ...evidence, keyAuthority: { ...evidence.keyAuthority, commit: "f".repeat(40) } } });
    const rejected = await publishPublicOwnershipCheckForTest({ repoRoot: product, expectedCommit: commit, platform: "win32", audit: () => ({ ok: true }), configPath, keyPath, trustModule, scanner: wrongScanner, apiCall });
    assert.equal(rejected.ok, false);
    assert.match(rejected.error.message, /key authority/);
  } finally { rmSync(product, { recursive: true, force: true }); rmSync(authority, { recursive: true, force: true }); rmSync(outside, { recursive: true, force: true }); }
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
