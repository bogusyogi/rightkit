import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";
import { fileURLToPath } from "node:url";
import test from "node:test";

const uploader = fileURLToPath(new URL("./upload-large.mjs", import.meta.url));

function fixtureFile() {
  const dir = mkdtempSync(path.join(os.tmpdir(), "right-upload-test-"));
  const file = path.join(dir, "Artifact.exe");
  writeFileSync(file, "artifact");
  return file;
}

function run(bucket) {
  return spawnSync(process.execPath, [uploader, fixtureFile(), "fixture/updates/windows/current/Artifact.exe", bucket], {
    encoding: "utf8",
    env: { ...process.env, RIGHT_RELEASE_UPLOAD_DRY_RUN: "1" },
  });
}

test("uploads public artifacts through wrangler remote R2 downloads bucket", () => {
  const result = run("public");
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /wrangler r2 object put rightapps-downloads\/fixture\/updates\/windows\/current\/Artifact\.exe/);
  assert.match(result.stdout, /--remote/);
  assert.doesNotMatch(result.stdout, /workers\.dev|multipart|heardright-models/i);
});

test("uploads private artifacts through wrangler remote R2 updates bucket", () => {
  const result = run("private");
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /wrangler r2 object put rightapps-updates\/fixture\/updates\/windows\/current\/Artifact\.exe/);
  assert.match(result.stdout, /--remote/);
});

test("refuses unknown bucket aliases", () => {
  const result = run("random");
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /expected public\|private/);
});

test("resolves a real package runner for wrangler on this machine", () => {
  const result = run("public");
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /^runner: pnpm dlx$/m);
});

test("fails loudly when no package runner exists instead of skipping the upload", () => {
  const result = spawnSync(
    process.execPath,
    [uploader, fixtureFile(), "fixture/installers/windows/current/Artifact.exe", "public"],
    {
      encoding: "utf8",
      env: {
        ...process.env,
        RIGHT_RELEASE_UPLOAD_DRY_RUN: "1",
        RIGHT_RELEASE_WRANGLER_RUNNER: "",
        PATH: path.join(os.tmpdir(), "right-upload-empty-path"),
        Path: path.join(os.tmpdir(), "right-upload-empty-path"),
        PATHEXT: process.env.PATHEXT ?? "",
      },
    },
  );
  assert.notEqual(result.status, 0, "a box with no runner must not report a successful upload");
  assert.match(result.stderr, /no package runner found/);
});
