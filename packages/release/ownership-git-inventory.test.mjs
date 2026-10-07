import assert from "node:assert/strict";
import { execFileSync } from "./background-process.mjs";
import { mkdtempSync, mkdirSync, readFileSync, rmSync, unlinkSync, writeFileSync } from "node:fs";
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
