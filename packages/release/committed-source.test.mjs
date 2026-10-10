import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { spawnSync } from "./background-process.mjs";
import { checkCommittedApp } from "./ownership.mjs";
import { acquireCommittedSourceForTest, assertCommittedCheckoutClean } from "./committed-source.mjs";

const git = (root, args, input) => {
  const result = spawnSync("git", ["-C", root, ...args], { encoding: "utf8", windowsHide: true, input });
  assert.equal(result.status, 0, result.stderr);
  return String(result.stdout).trim();
};
const trust = { enrolled: true, provenance: [] };
const manifest = (fingerprints) => ({ schema: 1, version: "1", capabilities: [{ package: "sample", capability: "sample", fingerprints }] });
function fixture(t) {
  const root = mkdtempSync(path.join(tmpdir(), "rightkit-committed-ownership-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  git(root, ["init", "-q"]);
  git(root, ["config", "user.email", "test@example.invalid"]);
  git(root, ["config", "user.name", "test"]);
  git(root, ["remote", "add", "origin", "https://github.com/test/repo.git"]);
  return root;
}
function commit(root, message = "fixture") {
  git(root, ["add", "-A"]);
  git(root, ["commit", "-qm", message]);
  return git(root, ["rev-parse", "HEAD"]);
}

test("committed scan detects owned source after checkout bytes change", (t) => {
  const root = fixture(t);
  writeFileSync(path.join(root, "src.rs"), "fn use_owned() { OWNED_MARKER; }\n");
  const commitId = commit(root);
  writeFileSync(path.join(root, "src.rs"), "working tree has no ownership marker\n");
  const result = checkCommittedApp(root, manifest([{ type: "source", id: "owned", pattern: "OWNED_MARKER" }]), { trust, expectedCommit: commitId });
  assert.equal(result.metadata.source, "committed-git-tree");
  assert.equal(result.metadata.commit, commitId);
  assert.equal(result.findings.length, 1);
  assert.equal(result.findings[0].file, "src.rs");
  assert.equal(result.violations.length, 1);
});

test("local Cargo and npm aliases resolve from committed manifests after checkout removal", (t) => {
  const root = fixture(t);
  mkdirSync(path.join(root, "crates", "helper"), { recursive: true });
  mkdirSync(path.join(root, "packages", "local"), { recursive: true });
  writeFileSync(path.join(root, "Cargo.toml"), '[package]\nname="app"\nversion="0.1.0"\n[dependencies]\nhelper={path="crates/helper"}\n');
  writeFileSync(path.join(root, "crates", "helper", "Cargo.toml"), '[package]\nname="helper"\nversion="0.1.0"\n');
  writeFileSync(path.join(root, "package.json"), JSON.stringify({ name: "app", dependencies: { local: "file:./packages/local", alias: "npm:@rightkit/special@1.0.0" } }));
  writeFileSync(path.join(root, "packages", "local", "package.json"), JSON.stringify({ name: "local" }));
  writeFileSync(path.join(root, "Cargo.lock"), 'version=4\n[[package]]\nname="app"\nversion="0.1.0"\ndependencies=["helper"]\n[[package]]\nname="helper"\nversion="0.1.0"\n');
  writeFileSync(path.join(root, "package-lock.json"), JSON.stringify({ lockfileVersion: 3, packages: {
    "": { name: "app", dependencies: { local: "file:./packages/local", alias: "npm:@rightkit/special@1.0.0" } },
    "packages/local": { name: "local", version: "0.0.0" }, "node_modules/local": { link: true, resolved: "packages/local", version: "0.0.0" },
    "node_modules/alias": { name: "@rightkit/special", version: "1.0.0" },
  } }));
  const commitId = commit(root);
  rmSync(path.join(root, "Cargo.toml"));
  rmSync(path.join(root, "package.json"));
  rmSync(path.join(root, "crates"), { recursive: true, force: true });
  rmSync(path.join(root, "packages"), { recursive: true, force: true });
  const result = checkCommittedApp(root, manifest([
    { type: "dependency", ecosystem: "cargo", name: "helper", id: "cargo-helper" },
    { type: "dependency", ecosystem: "npm", name: "@rightkit/special", id: "npm-alias" },
    { type: "dependency", ecosystem: "npm", name: "local", id: "npm-local" },
  ]), { trust, expectedCommit: commitId });
  assert.deepEqual(result.findings.map((finding) => finding.fingerprint).sort(), ["cargo-helper", "npm-alias", "npm-local"]);
  assert.equal(result.coverage.length, 0);
});

test("nested committed scan emits canonical repository-relative paths", (t) => {
  const root = fixture(t);
  mkdirSync(path.join(root, "apps", "desktop"), { recursive: true });
  writeFileSync(path.join(root, "apps", "desktop", "src.rs"), "fn owned() { OWNED_MARKER; }\n");
  writeFileSync(path.join(root, "engine.rs"), "fn unrelated() { OWNED_MARKER; }\n");
  const commitId = commit(root);
  const result = checkCommittedApp(path.join(root, "apps", "desktop"), manifest([{ type: "source", id: "owned", pattern: "OWNED_MARKER" }]), { trust, expectedCommit: commitId });
  assert.equal(result.metadata.pathPrefix, "apps/desktop/");
  assert.deepEqual(result.findings.map((finding) => finding.file), ["apps/desktop/src.rs"]);
});

test("committed source reader accepts trees with no scanner-relevant blobs", (t) => {
  const root = fixture(t);
  writeFileSync(path.join(root, "README.txt"), "plain documentation only\n");
  const commitId = commit(root);
  const result = checkCommittedApp(root, manifest([]), { trust, expectedCommit: commitId });
  assert.equal(result.metadata.commit, commitId);
  assert.equal(result.findings.length, 0);
  assert.equal(result.coverage.length, 0);
});

test("committed signed-exception bytes are read from Git object", (t) => {
  const root = fixture(t);
  writeFileSync(path.join(root, "src.rs"), "fn main() {}\n");
  mkdirSync(path.join(root, ".rightkit"), { recursive: true });
  const bytes = Buffer.from('{"exceptions":[{"payload":{"fingerprint":"old"}}]}\n');
  writeFileSync(path.join(root, ".rightkit", "ownership-exceptions.json"), bytes);
  const commitId = commit(root);
  writeFileSync(path.join(root, ".rightkit", "ownership-exceptions.json"), '{"exceptions":[]}\n');
  const result = checkCommittedApp(root, manifest([]), { trust, expectedCommit: commitId });
  assert.equal(result.metadata.exceptionDigest, createHash("sha256").update(bytes).digest("hex"));
  assert.match(result.problems.join(" "), /exception #0/);
});

test("committed symlinks, gitlinks, and oversized source fail closed", (t) => {
  const root = fixture(t);
  writeFileSync(path.join(root, "src.rs"), Buffer.alloc(3 * 1024 * 1024, 0x61));
  writeFileSync(path.join(root, "target.txt"), "link target\n");
  commit(root, "base");
  const linkBlob = git(root, ["hash-object", "-w", "--stdin"], "target.txt\n");
  const submoduleCommit = git(root, ["rev-parse", "HEAD"]);
  git(root, ["update-index", "--add", "--cacheinfo", `120000,${linkBlob},linked.rs`]);
  git(root, ["update-index", "--add", "--cacheinfo", `160000,${submoduleCommit},submodule`]);
  const commitId = git(root, ["commit", "-qm", "unsafe entries"]);
  const result = checkCommittedApp(root, manifest([]), { trust, expectedCommit: commitId });
  assert.equal(result.ok, false);
  assert.ok(result.coverage.some((entry) => entry.reason === "committed-symlink"));
  assert.ok(result.coverage.some((entry) => entry.reason === "committed-gitlink"));
  assert.ok(result.coverage.some((entry) => entry.reason === "source-file-over-2MiB"));
});

test("Git acquisition timeout is typed as incomplete committed coverage", (t) => {
  const root = fixture(t);
  const runner = () => ({ status: null, error: Object.assign(new Error("timed out"), { code: "ETIMEDOUT" }) });
  assert.throws(() => acquireCommittedSourceForTest(root, { runner }), (error) => {
    assert.equal(error.code, "COMMITTED_SOURCE_INCOMPLETE");
    assert.equal(error.coverage[0].status, "unscanned");
    return true;
  });
});

test("production clean-check rejects a dirty checkout before protected evidence", (t) => {
  const root = fixture(t);
  writeFileSync(path.join(root, "src.rs"), "fn main() {}\n");
  commit(root);
  writeFileSync(path.join(root, "src.rs"), "working tree changed\n");
  assert.throws(() => assertCommittedCheckoutClean(root), (error) => {
    assert.equal(error.code, "COMMITTED_SOURCE_INCOMPLETE");
    assert.match(error.message, /checkout has tracked or untracked changes/);
    return true;
  });
});

test("the clean-check refusal carries its check name and every offending path, rename sources excluded", () => {
  const runner = (status) => () => ({ status: 0, stdout: Buffer.from(status), stderr: Buffer.alloc(0) });
  assert.throws(() => assertCommittedCheckoutClean("/repo", { runner: runner("?? .right-release/locks/mac.lock.json\0R  new.rs\0old.rs\0?? stray.txt\0") }), (error) => {
    assert.match(error.message, /checkout has tracked or untracked changes/);
    assert.deepEqual(error.blocked, { check: "clean-checkout", paths: ["new.rs", "stray.txt"] });
    return true;
  });
});
