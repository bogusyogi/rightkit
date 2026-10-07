import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { spawnSync } from "node:child_process";
import { auditProtectedInstallForTest, runPrivateLocalOwnershipAdapterForTest, __testOnly } from "./private-local.mjs";
import { catalogDigest, sourceTreeDigest, validateProtectedReceipt } from "./receipt.mjs";
import { SCANNER_VERSION } from "../../packages/release/ownership.mjs";
import { nativeGitExecutable } from "./runtime-environment.mjs";

function git(root, ...args) {
  const result = spawnSync("git", ["-C", root, ...args], { encoding: "utf8", windowsHide: true });
  assert.equal(result.status, 0, result.stderr);
  return result.stdout.trim();
}
function fixture() {
  const root = mkdtempSync(path.join(tmpdir(), "rightkit-protected-adapter-"));
  git(root, "init", "-q");
  git(root, "config", "user.email", "test@example.invalid");
  git(root, "config", "user.name", "test");
  git(root, "remote", "add", "origin", "https://github.com/test/private-repo.git");
  writeFileSync(path.join(root, "source.txt"), "committed source bytes\n");
  writeFileSync(path.join(root, "rightkit-ownership.json"), JSON.stringify({ schema: 1, version: "1", capabilities: [] }));
  git(root, "add", ".");
  git(root, "commit", "-qm", "baseline");
  return root;
}
function goodReceipt(root) {
  const commit = git(root, "rev-parse", "HEAD");
  const tree = git(root, "rev-parse", "HEAD^{tree}");
  const listing = spawnSync("git", ["-C", root, "ls-tree", "-rz", "--full-tree", commit], { encoding: "buffer" }).stdout;
  return {
    schema: 1, type: "rightkit.protected-ownership-receipt", ok: true,
    repositoryIdentity: "https://github.com/test/private-repo", commit, tree,
    sourceDigest: sourceTreeDigest(listing), visibility: "private", registeredRoots: ["."],
    scannerVersion: SCANNER_VERSION, catalogVersion: "1", catalogDigest: catalogDigest(JSON.parse(readFileSync(path.join(root, "rightkit-ownership.json"), "utf8"))), contentDigest: "b".repeat(64),
    floors: { ok: true, digest: "c".repeat(64) }, protection: { ok: true, source: "os-protected-verifier" },
  };
}

test("protected receipt binds exact commit, tree, repository, bytes, floors, and roots", () => {
  const root = fixture();
  try {
    const receipt = goodReceipt(root);
    const listing = spawnSync("git", ["-C", root, "ls-tree", "-rz", "--full-tree", receipt.commit], { encoding: "buffer" }).stdout;
    const expected = { commit: receipt.commit, tree: receipt.tree, repositoryIdentity: receipt.repositoryIdentity, sourceDigest: sourceTreeDigest(listing), scannerVersion: receipt.scannerVersion, catalogVersion: receipt.catalogVersion, catalogDigest: receipt.catalogDigest };
    assert.deepEqual(validateProtectedReceipt(receipt, expected), { ok: true, problems: [] });
    for (const [key, value] of [["commit", "f".repeat(40)], ["repositoryIdentity", "https://evil.invalid/repo"], ["sourceDigest", "d".repeat(64)], ["catalogDigest", "e".repeat(64)]]) {
      const changed = { ...receipt, [key]: value };
      assert.equal(validateProtectedReceipt(changed, expected).ok, false, `${key} tampering must fail`);
    }
    assert.equal(validateProtectedReceipt({ ...receipt, floors: { ok: false, digest: "c".repeat(64) } }, expected).ok, false);
    assert.equal(validateProtectedReceipt({ ...receipt, registeredRoots: ["../outside"] }, expected).ok, false);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("private adapter accepts only its fixed verifier result and rejects malformed or failed output", () => {
  const root = fixture();
  try {
    const receipt = goodReceipt(root);
    const verifier = (exe, args, options) => {
      if (exe === nativeGitExecutable()) return spawnSync(exe, args, options);
      assert.ok(args.includes("--expected-commit"));
      assert.equal(options.timeout, 120000);
      assert.equal(options.env.GIT_NO_REPLACE_OBJECTS, "1");
      assert.equal(options.env.NODE_OPTIONS, undefined);
      return { status: 0, stdout: JSON.stringify(receipt), stderr: "" };
    };
    const options = { platform: "win32", verifierPath: "C:\\protected\\verifier.mjs", runtimePath: "C:\\protected\\node.exe", catalogPath: path.join(root, "rightkit-ownership.json"), audit: () => ({ ok: true }), runner: verifier };
    assert.equal(runPrivateLocalOwnershipAdapterForTest(root, options).ok, true);
    const malformed = (exe, args, opts) => exe === nativeGitExecutable() ? spawnSync(exe, args, opts) : ({ status: 0, stdout: "{}" });
    const rejected = (exe, args, opts) => exe === nativeGitExecutable() ? spawnSync(exe, args, opts) : ({ status: 3, stderr: "trust unavailable" });
    const timedOut = (exe, args, opts) => exe === nativeGitExecutable() ? spawnSync(exe, args, opts) : ({ status: null, error: { code: "ETIMEDOUT" } });
    assert.equal(runPrivateLocalOwnershipAdapterForTest(root, { ...options, runner: malformed }).error.type, "protected-receipt-invalid");
    assert.equal(runPrivateLocalOwnershipAdapterForTest(root, { ...options, runner: rejected }).error.type, "protected-verifier-rejected");
    assert.equal(runPrivateLocalOwnershipAdapterForTest(root, { ...options, runner: timedOut }).error.type, "protected-verifier-timeout");
    assert.equal(runPrivateLocalOwnershipAdapterForTest(root, { platform: "linux" }).error.type, "unsupported-platform");
    writeFileSync(path.join(root, "untracked.txt"), "not in commit\n");
    assert.equal(runPrivateLocalOwnershipAdapterForTest(root, options).error.type, "source-identity-invalid");
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("protected install audit command is fixed to provisioned roots and rejects other hosts", () => {
  assert.deepEqual(__testOnly.INSTALL_ROOTS, {
    win32: "C:\\Program Files\\Orthic Labs\\RightKit",
    darwin: "/Library/Application Support/Orthic Labs/RightKit",
  });
  assert.equal(auditProtectedInstallForTest({ platform: "linux" }).ok, false);
});
