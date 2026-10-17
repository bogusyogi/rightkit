import assert from "node:assert/strict";
import { existsSync, mkdtempSync, mkdirSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { generateKeyPairSync, sign } from "node:crypto";
import { spawnSync } from "./background-process.mjs";
import { catalogDigest, checkAppForTest, scanApp, SCANNER_VERSION } from "./ownership.mjs";
import { createCommittedOwnershipEvidenceForTest, createOwnershipEvidenceForTest, verifyOwnershipEvidenceForTest } from "./ownership-evidence.mjs";
import { sha256 } from "./ownership-approval.mjs";
import { __testOnly as authorityTestOnly } from "./ownership-key-authority.mjs";
import { createKeyEvent, eventHash, keyFingerprint } from "./ownership-key-log.mjs";
import { computePolicyBundle, runningPolicyBundle } from "./ownership-policy-bundle.mjs";

const releaseRoot = path.dirname(fileURLToPath(import.meta.url));
function trusted(root, { enrolled = true, protection = { ok: true }, roots = ["."], oneKey = false, minimumScannerVersion = "0.0.0", extraTrust = {} } = {}) {
  return () => ({
    enrolled,
    pending: !enrolled,
    protection,
    trust: {
      schema: 1,
      revokedKeyIds: [],
      hosts: oneKey ? { windows: { keyId: "windows-test-key", publicKeyPem: "test-key" } } : {},
      verifiedTrackingEvidence: [],
      minimumScannerVersion,
      minimumCatalogVersion: "0.0.0",
      provenance: [],
      repositories: [{ repositoryIdentity: "https://github.com/test/repo", roots, visibility: "private" }],
      ...extraTrust,
    },
  });
}
function git(root, ...args) {
  const result = spawnSync("git", ["-C", root, ...args], { encoding: "utf8", windowsHide: true });
  assert.equal(result.status, 0, result.stderr);
  return result.stdout.trim();
}

function keyPair(platform, deviceName) {
  const pair = generateKeyPairSync("ec", { namedCurve: "prime256v1" });
  const publicKeyPem = pair.publicKey.export({ type: "spki", format: "pem" });
  return { pair, record: { keyId: keyFingerprint(publicKeyPem), publicKeyPem, platform, deviceName } };
}

function signedEvent(log, input, signer) {
  const event = createKeyEvent(log, input);
  event.signature = { keyId: signer.record.keyId, host: signer.record.platform, value: sign("sha256", Buffer.from(canonicalJson(event.payload)), signer.pair.privateKey).toString("base64") };
  return event;
}

function readAuthorityForTest(trust) {
  return authorityTestOnly.readAuthority(trust, { file: path.join(trust.keyAuthority.checkoutPath, ".git", "host-state", "trust.json"), audit: () => {} });
}
function advanceCheckpoint(fixture) {
  const file = path.join(fixture.root, ".git", "host-state", "trust.json");
  mkdirSync(path.dirname(file), { recursive: true });
  writeFileSync(file, JSON.stringify(fixture.trust));
  authorityTestOnly.persistCheckpoint(fixture.trust, Buffer.from(`${JSON.stringify(fixture.log)}\n`), file, () => {});
}

function pinnedFixture({ includeDevice = true, unsignedAdd = false, revokeRoot = false, rootPin } = {}) {
  const root = realpathSync(mkdtempSync(path.join(tmpdir(), "rightkit-keylog-evidence-")));
  git(root, "init", "-q");
  git(root, "config", "user.email", "test@example.invalid");
  git(root, "config", "user.name", "test");
  git(root, "remote", "add", "origin", "https://github.com/test/repo.git");
  const rootKey = keyPair("windows", "root device");
  const deviceKey = keyPair("macos", "second device");
  const log = { schema: 1, events: [] };
  log.events.push(signedEvent(log, { operation: "root", subject: rootKey.record, signerKeyId: rootKey.record.keyId }, rootKey));
  const genesisHead = eventHash(log.events[0]);
  if (includeDevice) {
    const add = createKeyEvent(log, { operation: "add", subject: deviceKey.record, signerKeyId: rootKey.record.keyId });
    if (!unsignedAdd) add.signature = { keyId: rootKey.record.keyId, host: "windows", value: sign("sha256", Buffer.from(canonicalJson(add.payload)), rootKey.pair.privateKey).toString("base64") };
    log.events.push(add);
  }
  if (revokeRoot) log.events.push(signedEvent(log, { operation: "revoke", subject: { keyId: rootKey.record.keyId }, signerKeyId: rootKey.record.keyId }, rootKey));
  mkdirSync(path.join(root, ".rightkit"), { recursive: true });
  writeFileSync(path.join(root, ".rightkit", "ownership-keys.json"), `${JSON.stringify(log)}\n`);
  writeFileSync(path.join(root, "source.rs"), "fn main() {}\n");
  git(root, "add", ".");
  git(root, "commit", "-qm", "pinned key log");
  const logCommit = git(root, "rev-parse", "HEAD");
  const logTree = git(root, "rev-parse", "HEAD^{tree}");
  const trust = {
    schema: 1, rootFingerprint: rootPin ?? rootKey.record.keyId,
    keyCheckpoint: { sequence: 0, head: genesisHead }, revokedKeyIds: [],
    verifiedTrackingEvidence: [], minimumScannerVersion: "0.0.0", minimumCatalogVersion: "0.0.0", provenance: [],
    repositories: [{ repositoryIdentity: "https://github.com/test/repo", roots: ["."], visibility: "private" }],
  };
  const trustLoader = () => ({ enrolled: true, pending: false, protection: { ok: true }, trust });
  if (!unsignedAdd && !rootPin) advanceCheckpoint({ root, trust, log });
  const options = { scanRoot: root, manifestPath: path.join(releaseRoot, "rightkit-ownership.json"), trustLoader, readAuthorityForTest };
  return { root, rootKey, deviceKey, log, trust, options, logCommit, logTree };
}

test("release evidence binds scan to catalog, approvals, and immutable commit", () => {
  const root = realpathSync(mkdtempSync(path.join(tmpdir(), "rightkit-ownership-evidence-")));
  try {
    git(root, "init", "-q");
    git(root, "config", "user.email", "test@example.invalid");
    git(root, "config", "user.name", "test");
    git(root, "remote", "add", "origin", "https://github.com/test/repo.git");
    writeFileSync(path.join(root, "source.rs"), "fn main() {}\n");
    mkdirSync(path.join(root, ".rightkit"), { recursive: true });
    writeFileSync(path.join(root, ".rightkit", "ownership-exceptions.json"), '{"exceptions":[]}\n');
    git(root, "add", ".");
    git(root, "commit", "-qm", "baseline");

    const options = { manifestPath: path.join(releaseRoot, "rightkit-ownership.json"), trustLoader: trusted(root) };
    const built = createOwnershipEvidenceForTest(root, { ...options, scanRoot: root });
    assert.equal(built.ok, true);
    assert.equal(built.evidence.commit, git(root, "rev-parse", "HEAD"));
    assert.equal(built.evidence.visibility, "private");
    assert.equal(verifyOwnershipEvidenceForTest(built.evidence, root, { ...options, scanRoot: root }).ok, true);

    writeFileSync(path.join(root, "source.rs"), "fn changed_without_new_commit() {}\n");
    const dirty = verifyOwnershipEvidenceForTest(built.evidence, root, { ...options, scanRoot: root });
    assert.equal(dirty.ok, false);
    assert.match(dirty.reason, /contentDigest changed after build/);

    writeFileSync(path.join(root, "source.rs"), "fn changed_after_seal() {}\n");
    git(root, "add", ".");
    git(root, "commit", "-qm", "tampered source");
    const rejected = verifyOwnershipEvidenceForTest(built.evidence, root, { ...options, scanRoot: root });
    assert.equal(rejected.ok, false);
    assert.match(rejected.reason, /commit changed after build/);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("protected evidence scans exact committed blobs & binds source tree", () => {
  const root = realpathSync(mkdtempSync(path.join(tmpdir(), "rightkit-ownership-committed-evidence-")));
  try {
    git(root, "init", "-q");
    git(root, "config", "user.email", "test@example.invalid");
    git(root, "config", "user.name", "test");
    git(root, "remote", "add", "origin", "https://github.com/test/repo.git");
    const catalogPath = path.join(root, "catalog.json");
    writeFileSync(catalogPath, JSON.stringify({ schema: 1, version: "1", capabilities: [{ package: "sample", capability: "sample", tier: "block", fingerprints: [{ type: "source", id: "marker", pattern: "COMMITTED_ONLY_MARKER" }] }] }));
    writeFileSync(path.join(root, "source.rs"), "fn owned() { COMMITTED_ONLY_MARKER; }\n");
    git(root, "add", ".");
    git(root, "commit", "-qm", "committed evidence");
    writeFileSync(path.join(root, "source.rs"), "checkout bytes no longer contain marker\n");
    const checked = createCommittedOwnershipEvidenceForTest(root, { scanRoot: root, manifestPath: catalogPath, trustLoader: trusted(root) });
    assert.equal(checked.evidence.source, "committed-git-tree");
    assert.equal(checked.evidence.commit, git(root, "rev-parse", "HEAD"));
    assert.ok(checked.evidence.tree);
    assert.equal(checked.result.metadata.tree, checked.evidence.tree);
    assert.deepEqual(checked.result.findings.map((finding) => finding.file), ["source.rs"]);
    assert.equal(checked.evidence.scannerVersion, SCANNER_VERSION);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("release ownership fails closed when protected enrollment is absent", () => {
  assert.throws(() => createOwnershipEvidenceForTest(".", {
    trustLoader: () => ({ enrolled: false, pending: true, protection: { ok: false }, trust: null }),
  }), /protected ownership trust location was not verified/);
});

test("one-key enrollment fails closed and retains backlog findings", () => {
  const root = realpathSync(mkdtempSync(path.join(tmpdir(), "rightkit-ownership-pending-")));
  try {
    git(root, "init", "-q");
    git(root, "config", "user.email", "test@example.invalid");
    git(root, "config", "user.name", "test");
    git(root, "remote", "add", "origin", "https://github.com/test/repo.git");
    writeFileSync(path.join(root, "source.rs"), "fn main() {}\\n");
    git(root, "add", ".");
    git(root, "commit", "-qm", "baseline");
    const options = { manifestPath: path.join(releaseRoot, "rightkit-ownership.json"), trustLoader: trusted(root, { enrolled: false, oneKey: true }) };
    const pending = createOwnershipEvidenceForTest(root, {
      ...options,
      committed: true,
      allowPendingAdvisory: true,
      scanRoot: root,
      scan: (scanRoot, manifest) => ({
        ok: false, findings: [{ fingerprint: "pending-finding", file: "source.rs" }], violations: [{ fingerprint: "pending-finding", file: "source.rs" }],
        problems: ["approval enrollment pending; detected findings remain open backlog requests"], rows: [], covered: [], coverage: [],
        metadata: { repositoryIdentity: "https://github.com/test/repo", commit: git(root, "rev-parse", "HEAD"), catalogDigest: catalogDigest(manifest), contentDigests: [], coverage: [] },
      }),
    });
    assert.equal(pending.ok, false);
    assert.equal(pending.enrollmentPending, true);
    assert.match(pending.warning, /enrollment.*pending/);
    assert.equal(pending.enrollmentPending, true);
    assert.match(pending.warning, /^WARNING:/);
    assert.equal(pending.evidence.source, "enrollment-pending-advisory");
    assert.equal(pending.evidence.commit, git(root, "rev-parse", "HEAD"));
    assert.equal(pending.evidence.commit, pending.result.metadata.commit);
    assert.equal(pending.evidence.tree, undefined);
    assert.equal(pending.evidence.trustEnrolled, false);
    assert.equal(pending.evidence.floorOk, true);
    assert.equal(pending.evidence.visibility, "private");
    assert.equal(pending.result.violations[0].fingerprint, "pending-finding");
    assert.throws(() => createCommittedOwnershipEvidenceForTest(root, { ...options, scanRoot: root }), /requires both distinct/);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("absent trust fails closed without fabricated registry binding", () => {
  const root = realpathSync(mkdtempSync(path.join(tmpdir(), "rightkit-ownership-no-trust-")));
  try {
    git(root, "init", "-q");
    git(root, "config", "user.email", "test@example.invalid");
    git(root, "config", "user.name", "test");
    writeFileSync(path.join(root, "source.rs"), "fn main() {}\n");
    git(root, "add", ".");
    git(root, "commit", "-qm", "baseline");
    const pending = createOwnershipEvidenceForTest(root, {
      scanRoot: root,
      manifestPath: path.join(releaseRoot, "rightkit-ownership.json"),
      trustLoader: () => ({ enrolled: false, pending: true, protection: { ok: false, reason: "external trust file is absent" }, trust: null }),
      scan: (_scanRoot, manifest) => ({ ok: true, findings: [], violations: [], problems: [], rows: [], covered: [], coverage: [], metadata: { catalogDigest: catalogDigest(manifest), contentDigests: [], coverage: [] } }),
    });
    assert.equal(pending.ok, false);
    assert.equal(pending.enrollmentPending, true);
    assert.match(pending.warning, /enrollment.*pending/);
    assert.equal(pending.evidence.source, "enrollment-pending-advisory");
    assert.equal(pending.evidence.commit, git(root, "rev-parse", "HEAD"));
    assert.equal(pending.evidence.floorOk, false);
    assert.equal(pending.evidence.registrationDigest, null);
    assert.deepEqual(pending.evidence.registeredRoots, []);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("advisory evidence fails when scan source cannot produce a HEAD commit", () => {
  const options = {
    manifestPath: path.join(releaseRoot, "rightkit-ownership.json"),
    trustLoader: trusted(".", { enrolled: false }),
    committed: true,
    allowPendingAdvisory: true,
    scan: () => assert.fail("scan must not run without a source commit"),
  };
  for (const gitRunner of [
    () => ({ status: 0, stdout: "\n" }),
    () => ({ status: 128, stdout: "", stderr: "HEAD is unavailable" }),
  ]) {
    assert.throws(() => createOwnershipEvidenceForTest(".", { ...options, gitRunner }), /ownership evidence: (scan source did not produce a HEAD commit|git rev-parse HEAD failed)/);
  }
});

test("pending trust registration and floor problems remain diagnostics", () => {
  const root = realpathSync(mkdtempSync(path.join(tmpdir(), "rightkit-ownership-pending-registration-")));
  try {
    git(root, "init", "-q");
    git(root, "config", "user.email", "test@example.invalid");
    git(root, "config", "user.name", "test");
    git(root, "remote", "add", "origin", "https://github.com/test/repo.git");
    writeFileSync(path.join(root, "source.rs"), "fn main() {}\n");
    git(root, "add", ".");
    git(root, "commit", "-qm", "baseline");
    let scans = 0;
    const pending = createOwnershipEvidenceForTest(root, {
      scanRoot: root,
      manifestPath: path.join(releaseRoot, "rightkit-ownership.json"),
      trustLoader: trusted(root, { enrolled: false, roots: ["outside"], minimumScannerVersion: "99.0.0" }),
      scan: (_scanRoot, manifest) => {
        scans += 1;
        return { ok: false, findings: [], violations: [], problems: [], rows: [], covered: [], coverage: [], metadata: { repositoryIdentity: "https://github.com/test/repo", catalogDigest: catalogDigest(manifest), contentDigests: [], coverage: [] } };
      },
    });
    assert.equal(pending.ok, false);
    assert.equal(pending.enrollmentPending, true);
    assert.match(pending.warning, /enrollment.*pending/);
    assert.equal(scans, 2);
    assert.ok(pending.result.problems.some((problem) => /outside protected ownership roots/.test(problem)));
    assert.ok(pending.result.problems.some((problem) => /protected minimum|protected pinned|floor/.test(problem)));
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("malformed existing trust remains fail closed", () => {
  assert.throws(() => createOwnershipEvidenceForTest(".", {
    trustLoader: () => { throw new Error("ownership trust file schema is unsupported"); },
  }), /schema is unsupported/);
});

test("fully enrolled invalid approval stays a hard ownership failure", () => {
  const root = realpathSync(mkdtempSync(path.join(tmpdir(), "rightkit-ownership-invalid-approval-")));
  try {
    git(root, "init", "-q");
    git(root, "config", "user.email", "test@example.invalid");
    git(root, "config", "user.name", "test");
    git(root, "remote", "add", "origin", "https://github.com/test/repo.git");
    writeFileSync(path.join(root, "source.rs"), "fn main() {}\n");
    git(root, "add", ".");
    git(root, "commit", "-qm", "baseline");
    const manifestPath = path.join(releaseRoot, "rightkit-ownership.json");
    const pendingApproval = "approval signature verification failed (invalid signature)";
    const checked = createOwnershipEvidenceForTest(root, {
      scanRoot: root,
      manifestPath,
      trustLoader: trusted(root),
      scan: (_scanRoot, manifest) => ({
        ok: false, findings: [], violations: [], problems: [pendingApproval], rows: [], covered: [], coverage: [],
        metadata: { repositoryIdentity: "https://github.com/test/repo", commit: git(root, "rev-parse", "HEAD"), catalogDigest: catalogDigest(manifest), contentDigests: [], coverage: [] },
      }),
    });
    assert.equal(checked.enrollmentPending, undefined);
    assert.equal(checked.ok, false);
    assert.ok(checked.result.problems.includes(pendingApproval));
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("nested release app cannot narrow scan below every protected repository root", () => {
  const root = realpathSync(mkdtempSync(path.join(tmpdir(), "rightkit-ownership-roots-")));
  try {
    git(root, "init", "-q");
    git(root, "config", "user.email", "test@example.invalid");
    git(root, "config", "user.name", "test");
    git(root, "remote", "add", "origin", "git@github.com:test/repo.git");
    for (const name of ["engine", "apps/desktop"]) {
      mkdirSync(path.join(root, name), { recursive: true });
      writeFileSync(path.join(root, name, "source.rs"), "fn main() {}\\n");
    }
    git(root, "add", ".");
    git(root, "commit", "-qm", "baseline");
    const options = { manifestPath: path.join(releaseRoot, "rightkit-ownership.json"), trustLoader: trusted(root, { roots: ["engine", "apps/desktop"] }) };
    const calls = [];
    const scan = (scanRoot, manifest) => {
      calls.push(path.relative(root, scanRoot).replaceAll(path.sep, "/"));
      return {
        ok: true, findings: [], violations: [], problems: [], coverage: [], rows: [{ capability: "sample", status: "clean" }], covered: [{ fingerprint: "sample", file: "source.rs" }],
        metadata: {
          repositoryIdentity: "https://github.com/test/repo.git",
          commit: git(root, "rev-parse", "HEAD"),
          catalogDigest: catalogDigest(manifest),
          contentDigests: [{ path: "source.rs", digest: "a".repeat(64) }],
          coverage: [],
        },
      };
    };
    const result = createOwnershipEvidenceForTest(root, { ...options, scanRoot: path.join(root, "apps/desktop"), scan });
    assert.equal(result.ok, true);
    assert.deepEqual(calls.sort(), ["apps/desktop", "engine"]);
    assert.deepEqual(result.evidence.registeredRoots, ["engine", "apps/desktop"]);
    assert.deepEqual(result.result.covered.map((entry) => entry.file).sort(), ["apps/desktop/source.rs", "engine/source.rs"]);
    assert.equal(result.result.rows.length, 2);
    assert.ok(result.result.metadata.contentDigests.every((entry) => entry.path.includes("/")));
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("test seam defaults scan root to repository root", () => {
  const root = realpathSync(mkdtempSync(path.join(tmpdir(), "rightkit-ownership-default-root-")));
  try {
    git(root, "init", "-q");
    git(root, "config", "user.email", "test@example.invalid");
    git(root, "config", "user.name", "test");
    git(root, "remote", "add", "origin", "https://github.com/test/repo.git");
    writeFileSync(path.join(root, "source.rs"), "fn main() {}\n");
    git(root, "add", ".");
    git(root, "commit", "-qm", "baseline");
    const result = createOwnershipEvidenceForTest(root, { manifestPath: path.join(releaseRoot, "rightkit-ownership.json"), trustLoader: trusted(root) });
    assert.equal(result.ok, true);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("approval bytes changed after scan invalidate release evidence", () => {
  const root = realpathSync(mkdtempSync(path.join(tmpdir(), "rightkit-ownership-approvals-")));
  try {
    git(root, "init", "-q");
    git(root, "config", "user.email", "test@example.invalid");
    git(root, "config", "user.name", "test");
    git(root, "remote", "add", "origin", "https://github.com/test/repo.git");
    writeFileSync(path.join(root, "source.rs"), "fn main() {}\n");
    mkdirSync(path.join(root, ".rightkit"), { recursive: true });
    const approvals = path.join(root, ".rightkit", "ownership-exceptions.json");
    writeFileSync(approvals, '{"exceptions":[]}\n');
    git(root, "add", ".");
    git(root, "commit", "-qm", "baseline");
    const options = { manifestPath: path.join(releaseRoot, "rightkit-ownership.json"), trustLoader: trusted(root) };
    const built = createOwnershipEvidenceForTest(root, { ...options, scanRoot: root });
    writeFileSync(approvals, '{"exceptions":[{"signature":"changed"}]}\n');
    git(root, "add", ".");
    git(root, "commit", "-qm", "approval changed");
    const rejected = verifyOwnershipEvidenceForTest(built.evidence, root, { ...options, scanRoot: root });
    assert.equal(rejected.ok, false);
    assert.match(rejected.reason, /commit changed after build/);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("pinned log accepts committed device additions and forward evidence binds digest, head & root", () => {
  const fixture = pinnedFixture();
  try {
    const checked = createCommittedOwnershipEvidenceForTest(fixture.root, fixture.options);
    assert.equal(checked.ok, true);
    assert.equal(checked.evidence.rootFingerprint, fixture.rootKey.record.keyId);
    assert.equal(checked.evidence.keyCheckpoint.sequence, 1);
    assert.equal(checked.evidence.keyCheckpoint.head, eventHash(fixture.log.events[1]));
    assert.equal(checked.evidence.keyLogDigest, sha256(Buffer.from(`${JSON.stringify(fixture.log)}\n`)));
    const verified = verifyOwnershipEvidenceForTest(checked.evidence, fixture.root, { ...fixture.options, committed: true, requireCleanCheckout: true });
    assert.equal(verified.ok, true, verified.reason);
    for (const mutation of [
      { keyLogDigest: "f".repeat(64) },
      { keyCheckpoint: { ...checked.evidence.keyCheckpoint, head: "e".repeat(64) } },
      { rootFingerprint: "d".repeat(64) },
    ]) assert.equal(verifyOwnershipEvidenceForTest({ ...checked.evidence, ...mutation }, fixture.root, { ...fixture.options, committed: true, requireCleanCheckout: true }).ok, false);

    const later = signedEvent(fixture.log, { operation: "add", subject: keyPair("windows", "third device").record, signerKeyId: fixture.rootKey.record.keyId }, fixture.rootKey);
    fixture.log.events.push(later);
    writeFileSync(path.join(fixture.root, ".rightkit", "ownership-keys.json"), `${JSON.stringify(fixture.log)}\n`);
    git(fixture.root, "add", ".rightkit/ownership-keys.json");
    git(fixture.root, "commit", "-qm", "fast forward key log");
    assert.equal(verifyOwnershipEvidenceForTest(checked.evidence, fixture.root, fixture.options).ok, false);
    const forwarded = createCommittedOwnershipEvidenceForTest(fixture.root, fixture.options);
    assert.equal(forwarded.evidence.keyCheckpoint.sequence, 2);
    assert.equal(forwarded.evidence.keyLogDigest, sha256(Buffer.from(`${JSON.stringify(fixture.log)}\n`)));
  } finally { rmSync(fixture.root, { recursive: true, force: true }); }
});

test("pinned log rejects short chains, forks, forged roots & unsigned additions", () => {
  for (const mode of ["short", "fork", "root", "unsigned"]) {
    const fixture = pinnedFixture({ includeDevice: mode !== "short", unsignedAdd: mode === "unsigned", rootPin: mode === "root" ? "f".repeat(64) : undefined });
    try {
      if (mode === "short") fixture.trust.keyCheckpoint = { sequence: 1, head: "a".repeat(64) };
      if (mode === "fork") fixture.trust.keyCheckpoint = { sequence: 0, head: "a".repeat(64) };
      assert.throws(() => createCommittedOwnershipEvidenceForTest(fixture.root, fixture.options), /key log failed/);
    } finally { rmSync(fixture.root, { recursive: true, force: true }); }
  }
});

test("dirty forged worktree log cannot poison committed scan when checkout cleanliness is disabled", () => {
  const fixture = pinnedFixture();
  try {
    const committedBytes = Buffer.from(`${JSON.stringify(fixture.log)}\n`);
    const forged = { schema: 1, events: [] };
    writeFileSync(path.join(fixture.root, ".rightkit", "ownership-keys.json"), `${JSON.stringify(forged)}\n`);
    const checked = createCommittedOwnershipEvidenceForTest(fixture.root, { ...fixture.options, requireCleanCheckout: false });
    assert.equal(checked.ok, true);
    assert.equal(checked.evidence.keyLogDigest, sha256(committedBytes));
    assert.equal(checked.evidence.keyCheckpoint.head, eventHash(fixture.log.events[1]));
  } finally { rmSync(fixture.root, { recursive: true, force: true }); }
});

test("plain exception entry covers a finding in committed evidence even when the pinned key log has a revoked root (#262: no signature to check)", () => {
  const fixture = pinnedFixture({ revokeRoot: true });
  try {
    const manifestPath = path.join(fixture.root, "catalog.json");
    const manifest = { schema: 1, version: "1", capabilities: [{ package: "sample", capability: "sample", tier: "block", fingerprints: [{ type: "source", id: "marker", pattern: "REVOKED_KEY_LOG_MARKER" }] }] };
    writeFileSync(manifestPath, JSON.stringify(manifest));
    const line = "fn main() { REVOKED_KEY_LOG_MARKER; }";
    writeFileSync(path.join(fixture.root, "source.rs"), `${line}\n`);
    const record = { capability: "sample", package: "sample", reason: "test", owner: "test", date: "2026-10-17" };
    mkdirSync(path.join(fixture.root, ".rightkit"), { recursive: true });
    writeFileSync(path.join(fixture.root, ".rightkit", "ownership-exceptions.json"), JSON.stringify({ exceptions: [record] }));
    git(fixture.root, "add", ".");
    git(fixture.root, "commit", "-qm", "revoked approval fixture");
    const checked = createCommittedOwnershipEvidenceForTest(fixture.root, { ...fixture.options, manifestPath });
    assert.equal(checked.result.covered.length, 1);
    assert.equal(checked.result.violations.length, 0);
    assert.ok(!checked.result.problems.some((problem) => /signing key|signature/.test(problem)), checked.result.problems.join("; "));
  } finally { rmSync(fixture.root, { recursive: true, force: true }); }
});

test("ordinary evidence uses committed key history and refuses pending enrollment", () => {
  const fixture = pinnedFixture({ includeDevice: false });
  try {
    const result = createOwnershipEvidenceForTest(fixture.root, fixture.options);
    assert.equal(result.ok, false);
    assert.match(result.warning, /enrollment.*pending/);
    assert.equal(result.enrollmentPending, true);
    assert.equal(result.evidence.trustEnrolled, false);
    assert.equal(result.evidence.source, "enrollment-pending-advisory");
    assert.equal(result.evidence.commit, git(fixture.root, "rev-parse", "HEAD"));
    const committed = createCommittedOwnershipEvidenceForTest(fixture.root, fixture.options);
    assert.equal(committed.evidence.source, "enrollment-pending-advisory");
    assert.equal(committed.evidence.commit, result.evidence.commit);
    assert.equal(result.evidence.rootFingerprint, fixture.rootKey.record.keyId);
    assert.equal(result.evidence.keyCheckpoint.sequence, 0);
    assert.equal(result.evidence.keyLogDigest, sha256(Buffer.from(`${JSON.stringify(fixture.log)}\n`)));
    writeFileSync(path.join(fixture.root, ".rightkit", "ownership-keys.json"), '{"schema":1,"events":[]}\n');
    const dirty = createOwnershipEvidenceForTest(fixture.root, fixture.options);
    assert.equal(dirty.enrollmentPending, true);
    assert.equal(dirty.evidence.keyLogDigest, result.evidence.keyLogDigest);
    rmSync(path.join(fixture.root, ".rightkit", "ownership-keys.json"));
    const deleted = createOwnershipEvidenceForTest(fixture.root, fixture.options);
    assert.equal(deleted.enrollmentPending, true);
    assert.equal(deleted.evidence.keyLogDigest, result.evidence.keyLogDigest);
  } finally { rmSync(fixture.root, { recursive: true, force: true }); }
});

test("plain product exceptions are read from the committed tree under a shared workspace authority without copying its log", () => {
  const authority = pinnedFixture();
  const product = realpathSync(mkdtempSync(path.join(tmpdir(), "rightkit-product-repo-")));
  try {
    git(product, "init", "-q");
    git(product, "config", "user.email", "test@example.invalid");
    git(product, "config", "user.name", "test");
    git(product, "remote", "add", "origin", "https://github.com/test/product.git");
    const manifest = { schema: 1, version: "1", capabilities: [{ package: "sample", capability: "sample", tier: "block", fingerprints: [{ type: "source", id: "marker", pattern: "SHARED_AUTHORITY_MARKER" }] }] };
    const manifestPath = path.join(product, "catalog.json");
    writeFileSync(manifestPath, JSON.stringify(manifest));
    const line = "fn main() { SHARED_AUTHORITY_MARKER; }";
    writeFileSync(path.join(product, "source.rs"), `${line}\n`);
    mkdirSync(path.join(product, ".rightkit"), { recursive: true });
    const record = { capability: "sample", package: "sample", reason: "test", owner: "test", date: "2026-10-17" };
    const trust = { ...authority.trust, keyAuthority: { repositoryIdentity: "https://github.com/test/repo", checkoutPath: authority.root }, repositories: [{ repositoryIdentity: "https://github.com/test/product", roots: ["."], visibility: "private" }] };
    writeFileSync(path.join(product, ".rightkit", "ownership-exceptions.json"), JSON.stringify({ exceptions: [record] }));
    git(product, "add", ".");
    git(product, "commit", "-qm", "product source with shared approval");
    assert.equal(existsSync(path.join(product, ".rightkit", "ownership-keys.json")), false);
    const trustLoader = () => ({ enrolled: true, pending: false, protection: { ok: true }, trust });
    const options = { scanRoot: product, manifestPath, trustLoader, readAuthorityForTest };
    const built = createCommittedOwnershipEvidenceForTest(product, options);
    assert.equal(built.ok, true, built.result.problems.join("; "));
    assert.equal(built.result.covered.length, 1);
    assert.deepEqual(built.evidence.keyAuthority, { repositoryIdentity: "https://github.com/test/repo", commit: authority.logCommit, tree: authority.logTree });
    assert.equal(verifyOwnershipEvidenceForTest(built.evidence, product, { ...options, committed: true, requireCleanCheckout: true }).ok, true);

    const doctor = checkAppForTest(product, manifest, { trust: trustLoader(), readAuthorityForTest });
    assert.equal(doctor.enrollmentPending, false);
    assert.equal(doctor.covered.length, 1);
    assert.equal(doctor.metadata.keyLogDigest, built.evidence.keyLogDigest);

    for (const alteredTrust of [
      { ...trust, keyAuthority: { ...trust.keyAuthority, repositoryIdentity: "https://github.com/test/wrong" } },
      { ...trust, rootFingerprint: "f".repeat(64) },
      { ...trust, keyCheckpoint: { sequence: 8, head: "a".repeat(64) } },
      { ...trust, keyCheckpoint: { sequence: 0, head: "a".repeat(64) } },
    ]) assert.throws(() => createCommittedOwnershipEvidenceForTest(product, { ...options, trustLoader: () => ({ enrolled: true, pending: false, protection: { ok: true }, trust: alteredTrust }) }), /key authority failed|identity does not match|key log verification failed/);

    const third = keyPair("windows", "third device");
    authority.thirdKey = third;
    authority.log.events.push(signedEvent(authority.log, { operation: "add", subject: third.record, signerKeyId: authority.rootKey.record.keyId }, authority.rootKey));
    writeFileSync(path.join(authority.root, ".rightkit", "ownership-keys.json"), `${JSON.stringify(authority.log)}\n`);
    git(authority.root, "add", ".rightkit/ownership-keys.json");
    git(authority.root, "commit", "-qm", "workspace authority fast forward");
    advanceCheckpoint(authority);
    assert.equal(verifyOwnershipEvidenceForTest(built.evidence, product, { ...options, committed: true, requireCleanCheckout: true }).ok, false);
    const forwarded = createCommittedOwnershipEvidenceForTest(product, options);
    assert.equal(forwarded.ok, true);
    assert.notDeepEqual(forwarded.evidence.keyAuthority, built.evidence.keyAuthority);

    writeFileSync(path.join(authority.root, ".rightkit", "ownership-keys.json"), '{"schema":1,"events":[]}\n');
    const dirtyAuthority = createCommittedOwnershipEvidenceForTest(product, options);
    assert.equal(dirtyAuthority.ok, true);
    assert.equal(dirtyAuthority.evidence.keyLogDigest, forwarded.evidence.keyLogDigest);
    assert.deepEqual(dirtyAuthority.evidence.keyAuthority, forwarded.evidence.keyAuthority);

    const revokeDevice = signedEvent(authority.log, { operation: "revoke", subject: { keyId: authority.deviceKey.record.keyId }, signerKeyId: authority.rootKey.record.keyId }, authority.rootKey);
    authority.log.events.push(revokeDevice);
    authority.log.events.push(signedEvent(authority.log, { operation: "revoke", subject: { keyId: authority.thirdKey.record.keyId }, signerKeyId: authority.rootKey.record.keyId }, authority.rootKey));
    writeFileSync(path.join(authority.root, ".rightkit", "ownership-keys.json"), `${JSON.stringify(authority.log)}\n`);
    git(authority.root, "add", ".rightkit/ownership-keys.json");
    git(authority.root, "commit", "-qm", "revoke enrolled Mac key");
    advanceCheckpoint(authority);
    const revokedMac = checkAppForTest(product, manifest, { trust: trustLoader(), readAuthorityForTest });
    assert.equal(revokedMac.enrollmentPending, false);
    assert.equal(revokedMac.covered.length, 1);  // plain entries do not depend on any enrolled key
    assert.equal(revokedMac.violations.length, 0);
    const revokeRoot = signedEvent(authority.log, { operation: "revoke", subject: { keyId: authority.rootKey.record.keyId }, signerKeyId: authority.rootKey.record.keyId }, authority.rootKey);
    authority.log.events.push(revokeRoot);
    writeFileSync(path.join(authority.root, ".rightkit", "ownership-keys.json"), `${JSON.stringify(authority.log)}\n`);
    git(authority.root, "add", ".rightkit/ownership-keys.json");
    git(authority.root, "commit", "-qm", "revoke final active authority key");
    advanceCheckpoint(authority);
    assert.throws(() => checkAppForTest(product, manifest, { trust: trustLoader(), readAuthorityForTest }), /no active approval keys/);
    assert.throws(() => checkAppForTest(product, { schema: 1, version: "1", capabilities: [] }, { trust: trustLoader(), readAuthorityForTest }), /no active approval keys/);

    const tampered = JSON.parse(`${JSON.stringify(authority.log)}`);
    tampered.events[1].signature.value = `${tampered.events[1].signature.value.slice(0, -4)}AAAA`;
    writeFileSync(path.join(authority.root, ".rightkit", "ownership-keys.json"), JSON.stringify(tampered));
    git(authority.root, "add", ".rightkit/ownership-keys.json");
    git(authority.root, "commit", "-qm", "tampered committed authority log");
    assert.throws(() => createCommittedOwnershipEvidenceForTest(product, options), /key authority failed|key log verification failed/);
  } finally {
    rmSync(product, { recursive: true, force: true });
    rmSync(authority.root, { recursive: true, force: true });
  }
});

function bundleRepo() {
  const root = realpathSync(mkdtempSync(path.join(tmpdir(), "rightkit-ownership-bundle-")));
  git(root, "init", "-q");
  git(root, "config", "user.email", "test@example.invalid");
  git(root, "config", "user.name", "test");
  git(root, "remote", "add", "origin", "https://github.com/test/repo.git");
  writeFileSync(path.join(root, "source.rs"), "fn main() {}\n");
  git(root, "add", ".");
  git(root, "commit", "-qm", "baseline");
  return root;
}

test("pinned policy bundle: matching passes silently, any mismatch or compute failure only warns, unpinned trust stays valid", () => {
  const root = bundleRepo();
  try {
    const manifestPath = path.join(releaseRoot, "rightkit-ownership.json");
    const manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
    const running = runningPolicyBundle({ scannerVersion: SCANNER_VERSION, manifest });
    const build = (extraTrust, extra = {}) => createOwnershipEvidenceForTest(root, { manifestPath, scanRoot: root, trustLoader: trusted(root, { extraTrust }), ...extra });

    assert.equal(build({ policyBundle: running }).ok, true, "pinned bundle equal to the running one passes");
    assert.equal(build({}).ok, true, "trust with no policyBundle is unpinned");

    const wrong = build({ policyBundle: { ...running, digest: "0".repeat(64) } });
    assert.equal(wrong.ok, true, "a stale pinned bundle is a warning, never a block");
    assert.equal(wrong.evidence.floorOk, true);
    assert.match(wrong.warnings.join("\n"), /ownership policy bundle .* differs from the bundle pinned in protected trust/);
    assert.equal(build({ policyBundle: running }).warnings, undefined, "no drift, no warning");

    // Scanner source edited while the catalog is kept: the running bundle digest no longer matches the pin.
    const edited = build({ policyBundle: running }, { policyBundleProvider: ({ scannerVersion, manifest: m }) => computePolicyBundle({ scannerVersion, manifest: m, files: running.files.map((f, i) => i ? f : { ...f, sha256: "1".repeat(64) }) }) });
    assert.equal(edited.ok, true);
    assert.match(edited.warnings.join("\n"), /policy bundle/);
    // Changed allowlist or registry content moves the digest too.
    const allowlisted = build({ policyBundle: running }, { policyBundleProvider: ({ scannerVersion, manifest: m }) => computePolicyBundle({ scannerVersion, manifest: m, allowlist: { schema: 1, global: ["npm:evil"], repositories: [] } }) });
    assert.equal(allowlisted.ok, true);
    assert.match(allowlisted.warnings.join("\n"), /policy bundle/);
    const failing = build({ policyBundle: running }, { policyBundleProvider: () => { throw new Error("boom"); } });
    assert.equal(failing.ok, true);
    assert.match(failing.warnings.join("\n"), /could not be computed: boom/);

    // Previously sealed evidence stays valid across a bundle mismatch too.
    const sealed = build({ policyBundle: running });
    const verified = verifyOwnershipEvidenceForTest(sealed.evidence, root, { manifestPath, scanRoot: root, trustLoader: trusted(root, { extraTrust: { policyBundle: { ...running, digest: "2".repeat(64) } } }) });
    assert.equal(verified.ok, true);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("pinned policy bundle mismatch stays advisory while approval enrollment is pending", () => {
  const root = bundleRepo();
  try {
    const manifestPath = path.join(releaseRoot, "rightkit-ownership.json");
    const running = runningPolicyBundle({ scannerVersion: SCANNER_VERSION, manifest: JSON.parse(readFileSync(manifestPath, "utf8")) });
    const result = createOwnershipEvidenceForTest(root, { manifestPath, scanRoot: root, trustLoader: trusted(root, { enrolled: false, extraTrust: { policyBundle: { ...running, digest: "3".repeat(64) } } }) });
    assert.equal(result.ok, false);
    assert.equal(result.enrollmentPending, true);
    assert.match(result.warnings.join("\n"), /policy bundle/);
  } finally { rmSync(root, { recursive: true, force: true }); }
});
