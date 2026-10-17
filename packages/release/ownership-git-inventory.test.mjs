import assert from "node:assert/strict";
import { execFileSync } from "./background-process.mjs";
import { mkdtempSync, mkdirSync, readFileSync, rmSync, statSync, unlinkSync, utimesSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { test } from "node:test";
import { clearGitListedInventoryCacheForTests, gitListedInventory } from "./ownership-git-inventory.mjs";

function fixture() {
  const root = mkdtempSync(path.join(os.tmpdir(), "ownership-inventory-"));
  const git = (...args) => execFileSync("git", args, { cwd: root, stdio: "ignore" });
  git("init", "-q"); git("config", "user.email", "inventory@example.test"); git("config", "user.name", "Inventory Test");
  return { root, git, close: () => rmSync(root, { recursive: true, force: true }) };
}

test("Git-listed inventory bounds scope, hashes mutations, reports gaps & reuses clean blobs", async () => {
  clearGitListedInventoryCacheForTests();
  const f = fixture();
  try {
    mkdirSync(path.join(f.root, "src"));
    mkdirSync(path.join(f.root, "node_modules", "huge"), { recursive: true });
    writeFileSync(path.join(f.root, "src", "a.js"), "const a = 1;\n");
    writeFileSync(path.join(f.root, "src", "space name.js"), "const space = 1;\n");
    writeFileSync(path.join(f.root, "node_modules", "huge", "ignored.js"), "ignored\n");
    writeFileSync(path.join(f.root, ".gitignore"), "node_modules/\n");
    f.git("add", "."); f.git("commit", "-qm", "seed");

    const relevant = (rel) => rel.endsWith(".js");
    let result = await gitListedInventory(f.root, { relevant });
    assert.deepEqual(result.files.map((x) => x.rel).sort(), [".gitignore", "src/a.js", "src/space name.js"]);
    assert.equal(result.files.find((x) => x.rel === ".gitignore").bytes, undefined);
    const initial = result.files.find((x) => x.rel === "src/a.js");
    assert.equal(initial.digest, `git:${initial.oid}`);
    const cached = await gitListedInventory(f.root, { relevant });
    assert.equal(cached.files.find((x) => x.rel === "src/a.js").bytes, initial.bytes);

    writeFileSync(path.join(f.root, "src", "a.js"), "const a = 2;\n");
    writeFileSync(path.join(f.root, "src", "new.js"), "const fresh = true;\n");
    result = await gitListedInventory(f.root, { relevant });
    const changed = result.files.find((x) => x.rel === "src/a.js");
    const fresh = result.files.find((x) => x.rel === "src/new.js");
    assert.notEqual(changed.digest, initial.digest);
    assert.match(changed.digest, /^[0-9a-f]{64}$/);
    assert.equal(changed.bytes.toString(), "const a = 2;\n");
    assert.equal(fresh.oid, undefined);

    f.git("add", "src/new.js");
    result = await gitListedInventory(f.root, { relevant });
    assert.ok(result.files.find((x) => x.rel === "src/new.js").oid);
    unlinkSync(path.join(f.root, "src", "space name.js"));
    result = await gitListedInventory(f.root, { relevant });
    assert.ok(result.coverage.some((x) => x.path === "src/space name.js" && x.reason === "deleted"));
    assert.equal(readFileSync(path.join(f.root, "src", "a.js"), "utf8"), "const a = 2;\n");
  } finally { f.close(); clearGitListedInventoryCacheForTests(); }
});

test("file-size limits skip clean & dirty reads with typed coverage", () => {
  clearGitListedInventoryCacheForTests();
  const f = fixture();
  try {
    writeFileSync(path.join(f.root, "large.js"), "0123456789abcdef\n");
    f.git("add", "large.js"); f.git("commit", "-qm", "seed");
    let result = gitListedInventory(f.root, { relevant: () => true, maxFileBytes: 8 });
    assert.equal(result.files[0].bytes, undefined);
    assert.ok(result.coverage.some((x) => x.path === "large.js" && x.reason === "file-over-size-limit"));
    writeFileSync(path.join(f.root, "large.js"), "0123456789abcdef more\n");
    result = gitListedInventory(f.root, { relevant: () => true, maxFileBytes: 8 });
    assert.equal(result.files[0].bytes, undefined);
    assert.ok(result.coverage.some((x) => x.path === "large.js" && x.reason === "file-over-size-limit"));
  } finally { f.close(); clearGitListedInventoryCacheForTests(); }
});

test("nested app roots keep listing & dirty paths app-relative", () => {
  clearGitListedInventoryCacheForTests();
  const f = fixture();
  try {
    const app = path.join(f.root, "apps", "demo");
    mkdirSync(app, { recursive: true });
    writeFileSync(path.join(app, "nested.js"), "const nested = 1;\n");
    f.git("add", "."); f.git("commit", "-qm", "seed");
    let result = gitListedInventory(app, { repositoryRoot: f.root, relevant: () => true });
    assert.deepEqual(result.files.map((x) => x.rel), ["nested.js"]);
    assert.equal(result.files[0].size, result.files[0].bytes.length);

    writeFileSync(path.join(app, "nested.js"), "const staged = 1;\n");
    f.git("add", "apps/demo/nested.js");
    result = gitListedInventory(app, { repositoryRoot: f.root, relevant: () => true });
    assert.deepEqual(result.files.map((x) => x.rel), ["nested.js"]);
    assert.equal(result.files[0].bytes.toString(), "const staged = 1;\n");
    assert.equal(result.files[0].digest, `git:${result.files[0].oid}`);

    writeFileSync(path.join(app, "nested.js"), "const dirty = 1;\n");
    result = gitListedInventory(app, { repositoryRoot: f.root, relevant: () => true });
    assert.deepEqual(result.files.map((x) => x.rel), ["nested.js"]);
    assert.equal(result.files[0].bytes.toString(), "const dirty = 1;\n");
    assert.match(result.files[0].digest, /^[0-9a-f]{64}$/);
  } finally { f.close(); clearGitListedInventoryCacheForTests(); }
});

test("clean irrelevant regular files retain Git-listed path records without loaded bytes", () => {
  clearGitListedInventoryCacheForTests();
  const f = fixture();
  try {
    writeFileSync(path.join(f.root, "asset.bin"), "opaque asset\n");
    writeFileSync(path.join(f.root, "source.js"), "const source = true;\n");
    f.git("add", "."); f.git("commit", "-qm", "seed");
    const result = gitListedInventory(f.root, { relevant: (rel) => rel.endsWith(".js") });
    const asset = result.files.find((item) => item.rel === "asset.bin");
    assert.ok(asset, "irrelevant paths must remain available for listed coverage");
    assert.equal(asset.bytes, undefined);
    assert.match(asset.oid, /^[0-9a-f]{40,64}$/);
    assert.equal(asset.digest, `git:${asset.oid}`);
  } finally { f.close(); clearGitListedInventoryCacheForTests(); }
});

test("dirty irrelevant deletions and type changes still produce coverage", () => {
  clearGitListedInventoryCacheForTests();
  const f = fixture();
  try {
    writeFileSync(path.join(f.root, "removed.bin"), "remove me\n");
    writeFileSync(path.join(f.root, "directory.bin"), "replace with directory\n");
    f.git("add", "."); f.git("commit", "-qm", "seed");
    unlinkSync(path.join(f.root, "removed.bin"));
    unlinkSync(path.join(f.root, "directory.bin"));
    mkdirSync(path.join(f.root, "directory.bin"));
    const result = gitListedInventory(f.root, { relevant: () => false });
    assert.ok(result.coverage.some((item) => item.path === "removed.bin" && item.reason === "deleted"));
    assert.ok(result.coverage.some((item) => item.path === "directory.bin" && item.reason === "not-regular-file"));
  } finally { f.close(); clearGitListedInventoryCacheForTests(); }
});

test("protected paths remain relevant even when the caller relevance filter rejects them", () => {
  clearGitListedInventoryCacheForTests();
  const f = fixture();
  try {
    writeFileSync(path.join(f.root, "protected.bin"), "protected bytes\n");
    f.git("add", "protected.bin"); f.git("commit", "-qm", "seed");
    const result = gitListedInventory(f.root, { relevant: () => false, protectedPaths: ["protected.bin"] });
    const protectedFile = result.files.find((item) => item.rel === "protected.bin");
    assert.ok(protectedFile);
    assert.equal(protectedFile.bytes.toString("utf8"), "protected bytes\n");
    assert.equal(protectedFile.size, protectedFile.bytes.length);
    assert.equal(protectedFile.digest, `git:${protectedFile.oid}`);
  } finally { f.close(); clearGitListedInventoryCacheForTests(); }
});

const stamp = (file, seconds) => utimesSync(file, seconds, seconds);
const ago = (seconds) => Math.floor(Date.now() / 1000) - seconds;
const digestOf = (root, rel) => gitListedInventory(root, { relevant: () => true }).files.find((x) => x.rel === rel).digest;

test("a dirty input is reused only while its size & mtime are unchanged", () => {
  clearGitListedInventoryCacheForTests();
  const f = fixture();
  try {
    const file = path.join(f.root, "settled.txt");
    const settled = ago(3600);
    writeFileSync(file, "aaaa\n"); stamp(file, settled);
    const first = digestOf(f.root, "settled.txt");

    // Same size & mtime: the verified copy is reused, so the rewritten bytes are not read.
    writeFileSync(file, "bbbb\n"); stamp(file, settled);
    assert.equal(digestOf(f.root, "settled.txt"), first);

    // A moved mtime invalidates it.
    writeFileSync(file, "bbbb\n"); stamp(file, ago(1800));
    const second = digestOf(f.root, "settled.txt");
    assert.notEqual(second, first);

    // A changed size invalidates it even at the same mtime.
    writeFileSync(file, "longer content\n"); stamp(file, ago(1800));
    assert.notEqual(digestOf(f.root, "settled.txt"), second);
    assert.equal(statSync(file).size, "longer content\n".length);
  } finally { f.close(); clearGitListedInventoryCacheForTests(); }
});

test("a dirty input modified inside the racy window is never kept", () => {
  clearGitListedInventoryCacheForTests();
  const f = fixture();
  try {
    const file = path.join(f.root, "fresh.txt");
    const now = ago(0);
    writeFileSync(file, "aaaa\n"); stamp(file, now);
    const first = digestOf(f.root, "fresh.txt");
    writeFileSync(file, "bbbb\n"); stamp(file, now);
    assert.notEqual(digestOf(f.root, "fresh.txt"), first, "a same-size rewrite at a fresh mtime must be read again");
  } finally { f.close(); clearGitListedInventoryCacheForTests(); }
});

test("reuse never serves a deleted or replaced input and clean blobs keep their Git digest", () => {
  clearGitListedInventoryCacheForTests();
  const f = fixture();
  try {
    const file = path.join(f.root, "gone.txt");
    // Tracked, so its deletion is visible to git (an untracked file that vanishes is simply not listed).
    writeFileSync(file, "aaaa\n"); f.git("add", "gone.txt"); f.git("commit", "-qm", "gone"); stamp(file, ago(3600));
    assert.ok(digestOf(f.root, "gone.txt"));
    unlinkSync(file);
    const result = gitListedInventory(f.root, { relevant: () => true });
    assert.equal(result.files.find((x) => x.rel === "gone.txt"), undefined);
    assert.ok(result.coverage.some((x) => x.path === "gone.txt" && x.reason === "deleted"));
    writeFileSync(path.join(f.root, "clean.txt"), "clean\n");
    f.git("add", "clean.txt"); f.git("commit", "-qm", "seed");
    const clean = gitListedInventory(f.root, { relevant: () => true }).files.find((x) => x.rel === "clean.txt");
    assert.equal(clean.digest, `git:${clean.oid}`);
  } finally { f.close(); clearGitListedInventoryCacheForTests(); }
});
