import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { syncCommand } from "./sync.mjs";
import { makeTempRepo, PUBLIC_DEPS, PRIVATE_DEPS, SAMPLE_MANIFEST } from "./test-support.mjs";

test("sync refuses a private repo and writes nothing", async () => {
  const dir = makeTempRepo(SAMPLE_MANIFEST);
  const result = await syncCommand(dir, { deps: PRIVATE_DEPS });
  assert.equal(result.ok, false);
  assert.equal(result.stage, "visibility");
  assert.equal(fs.existsSync(path.join(dir, ".github", "workflows")), false);
});

test("sync writes one rendered file per lane for a public repo", async () => {
  const dir = makeTempRepo(SAMPLE_MANIFEST);
  const result = await syncCommand(dir, { deps: PUBLIC_DEPS });
  assert.equal(result.ok, true);
  for (const filename of ["ci.yml", "package-smoke.yml"]) {
    assert.ok(fs.existsSync(path.join(dir, ".github", "workflows", filename)), `${filename} was not written`);
  }
});

test("sync --dry-run plans changes without writing files", async () => {
  const dir = makeTempRepo(SAMPLE_MANIFEST);
  const result = await syncCommand(dir, { deps: PUBLIC_DEPS, dryRun: true });
  assert.equal(result.ok, true);
  assert.ok(result.changes.some((c) => c.action === "create"));
  assert.equal(fs.existsSync(path.join(dir, ".github", "workflows")), false);
});

test("sync is idempotent: a second run reports no changes", async () => {
  const dir = makeTempRepo(SAMPLE_MANIFEST);
  await syncCommand(dir, { deps: PUBLIC_DEPS });
  const second = await syncCommand(dir, { deps: PUBLIC_DEPS });
  assert.ok(second.changes.every((c) => c.action === "unchanged"));
});

test("sync removes a workflow file for a lane dropped from the manifest", async () => {
  const dir = makeTempRepo(SAMPLE_MANIFEST);
  await syncCommand(dir, { deps: PUBLIC_DEPS });
  fs.writeFileSync(path.join(dir, ".rightgit.json"), `${JSON.stringify({ ...SAMPLE_MANIFEST, lanes: ["ci"] }, null, 2)}\n`, "utf8");
  const result = await syncCommand(dir, { deps: PUBLIC_DEPS });
  assert.equal(result.ok, true);
  assert.equal(fs.existsSync(path.join(dir, ".github", "workflows", "package-smoke.yml")), false);
  assert.ok(fs.existsSync(path.join(dir, ".github", "workflows", "ci.yml")));
});

test("sync reports manifest errors instead of writing files", async () => {
  const dir = makeTempRepo(null);
  const result = await syncCommand(dir, { deps: PUBLIC_DEPS });
  assert.equal(result.ok, false);
  assert.equal(result.stage, "manifest");
});
