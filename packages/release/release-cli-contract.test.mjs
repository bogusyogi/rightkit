import assert from "node:assert/strict";
import { mkdtempSync, readFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import test from "node:test";

const root = path.dirname(fileURLToPath(import.meta.url));
const cli = path.join(root, "cli", "right-release.mjs");
const build = path.join(root, "build-release.mjs");
const upload = path.join(root, "upload-release.mjs");
const signWindows = path.join(root, "sign-windows.mjs");

test("CLI routes build and release only to the build state machine", () => {
  const source = readFileSync(cli, "utf8");
  assert.match(source, /\["build",\s*"build-release\.mjs"\]/);
  assert.match(source, /\["release",\s*"build-release\.mjs"\]/);
});

test("CLI routes upload and legacy publish only to the upload state machine", () => {
  const source = readFileSync(cli, "utf8");
  assert.match(source, /\["upload",\s*"upload-release\.mjs"\]/);
  assert.match(source, /\["publish",\s*"upload-release\.mjs"\]/);
  assert.doesNotMatch(source, /first === "publish"[\s\S]{0,500}run\("release\.mjs"/);
});

test("upload dry-run cannot write a verified marker", () => {
  const source = readFileSync(upload, "utf8");
  assert.match(source, /if \(!dryRun\)\s+writeJson\(verifiedMarker,/);
});

test("build is tier-neutral and rejects a tier before touching a repository", () => {
  const result = spawnSync(process.execPath, [build, "--platform", "win", "--tier", "patch"], {
    cwd: mkdtempSync(path.join(os.tmpdir(), "right-build-cli-")),
    encoding: "utf8",
  });
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /build is tier-neutral/i);
});

test("build enforces primary-checkout and build-input cleanliness guards", () => {
  const source = readFileSync(build, "utf8");
  assert.match(source, /assertPrimaryReleaseCheckout\(repoRoot\)/);
  assert.match(source, /dirtyBuildInputs\(repoRoot,/);
  assert.match(source, /dirty files can change the packaged app/i);
});

test("upload enforces the same primary-checkout vault", () => {
  const source = readFileSync(upload, "utf8");
  assert.match(source, /assertPrimaryReleaseCheckout\(repoRoot\)/);
});

test("upload requires an explicit patch or update tier before reading a release", () => {
  const result = spawnSync(process.execPath, [upload, "--platform", "win", "--release", "fixture-1.0.0-deadbeef"], {
    cwd: mkdtempSync(path.join(os.tmpdir(), "right-upload-cli-")),
    encoding: "utf8",
  });
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /tier is required.*patch\|update/i);
});

test("Windows upload trust verification uses signtool instead of PowerShell modules", () => {
  const uploadSource = readFileSync(upload, "utf8");
  const signingSource = readFileSync(signWindows, "utf8");
  assert.match(uploadSource, /SIGN_WINDOWS/);
  assert.match(uploadSource, /--verify-only/);
  assert.doesNotMatch(uploadSource, /Get-AuthenticodeSignature/);
  assert.match(signingSource, /verifyOnly/);
  assert.match(signingSource, /\["verify", "\/pa", "\/v", file\]/);
});

test("upload uses only the pinned pnpm runner", () => {
  const source = readFileSync(upload, "utf8");
  assert.match(source, /spawnSync\("pnpm", \["dlx", "wrangler@4"/);
  assert.doesNotMatch(source, /\bnpx\b/);
});
