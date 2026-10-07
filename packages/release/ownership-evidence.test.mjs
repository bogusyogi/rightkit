import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { spawnSync } from "./background-process.mjs";
import { catalogDigest } from "./ownership.mjs";
import { createOwnershipEvidenceForTest, verifyOwnershipEvidenceForTest } from "./ownership-evidence.mjs";

const releaseRoot = path.dirname(fileURLToPath(import.meta.url));
function trusted(root, { enrolled = true, protection = { ok: true }, roots = ["."] } = {}) {
  return () => ({
    enrolled,
    pending: !enrolled,
    protection,
    trust: {
      schema: 1,
      revokedKeyIds: [],
      verifiedTrackingEvidence: [],
      minimumScannerVersion: "0.0.0",
      minimumCatalogVersion: "0.0.0",
      provenance: [],
      repositories: [{ repositoryIdentity: "https://github.com/test/repo", roots, visibility: "private" }],
    },
  });
}
function git(root, ...args) {
  const result = spawnSync("git", ["-C", root, ...args], { encoding: "utf8", windowsHide: true });
  assert.equal(result.status, 0, result.stderr);
  return result.stdout.trim();
}

test("release evidence binds scan to catalog, approvals, and immutable commit", () => {
  const root = mkdtempSync(path.join(tmpdir(), "rightkit-ownership-evidence-"));
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

test("release ownership fails closed when protected enrollment is absent", () => {
  assert.throws(() => createOwnershipEvidenceForTest(".", {
    trustLoader: () => ({ enrolled: false, pending: true, protection: { ok: false }, trust: null }),
  }), /protected ownership trust location was not verified/);
});

test("pending key enrollment keeps protected floors and registry active without unsigned authority", () => {
  const root = mkdtempSync(path.join(tmpdir(), "rightkit-ownership-pending-"));
  try {
    git(root, "init", "-q");
    git(root, "config", "user.email", "test@example.invalid");
    git(root, "config", "user.name", "test");
    git(root, "remote", "add", "origin", "https://github.com/test/repo.git");
    writeFileSync(path.join(root, "source.rs"), "fn main() {}\\n");
    git(root, "add", ".");
    git(root, "commit", "-qm", "baseline");
    const options = { manifestPath: path.join(releaseRoot, "rightkit-ownership.json"), trustLoader: trusted(root, { enrolled: false }) };
    const pending = createOwnershipEvidenceForTest(root, { ...options, scanRoot: root });
    assert.equal(pending.ok, true);
    assert.equal(pending.evidence.trustEnrolled, false);
    assert.equal(pending.evidence.floorOk, true);
    assert.equal(pending.evidence.visibility, "private");
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("nested release app cannot narrow scan below every protected repository root", () => {
  const root = mkdtempSync(path.join(tmpdir(), "rightkit-ownership-roots-"));
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
  const root = mkdtempSync(path.join(tmpdir(), "rightkit-ownership-default-root-"));
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
  const root = mkdtempSync(path.join(tmpdir(), "rightkit-ownership-approvals-"));
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
