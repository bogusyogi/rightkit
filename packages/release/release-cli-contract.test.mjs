import assert from "node:assert/strict";
import { mkdtempSync, readFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import vm from "node:vm";
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

test("CLI advertises build-only --skip-checks", () => {
  const source = readFileSync(cli, "utf8");
  assert.match(source, /build \[--platform mac\|win\] \[--skip-checks\]/);
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

test("GitHub upload delegates platform trust verification to the shared release lane", () => {
  const uploadSource = readFileSync(upload, "utf8");
  const githubSource = readFileSync(path.join(root, "github-release.mjs"), "utf8");
  const signingSource = readFileSync(signWindows, "utf8");
  assert.match(uploadSource, /publishGitHubRelease/);
  assert.match(githubSource, /sign-windows\.mjs/);
  assert.match(githubSource, /--verify-only/);
  assert.doesNotMatch(uploadSource, /CLOUDFLARE_API_TOKEN|wrangler|R2/i);
  assert.match(signingSource, /verifyOnly/);
  assert.match(signingSource, /\["verify", "\/pa", "\/v", file\]/);
});

test("Windows signer verification accepts CRLF subjects and retains identity checks", () => {
  const source = readFileSync(signWindows, "utf8");
  const functionSource = source.slice(source.indexOf("function assertVerification"), source.indexOf("\nfunction fileEvidence"));
  const assertVerification = vm.runInNewContext(`(${functionSource})`, {
    fail(message) { throw new Error(message); },
  });
  const valid = "Successfully verified\r\n\tIssued to: Damned Ventures LLC  \r\nTimestamp: present\r\n";
  assert.equal(JSON.stringify(assertVerification("fixture.exe", valid)), JSON.stringify({
    authenticode: "Valid",
    subject: "CN=Damned Ventures LLC",
    timestampPresent: true,
  }));
  assert.throws(
    () => assertVerification("fixture.exe", valid.replace("Damned Ventures LLC", "Other Company")),
    /subject must be Damned Ventures LLC/,
  );
  assert.throws(
    () => assertVerification("fixture.exe", valid.replace("Damned Ventures LLC", "damned ventures llc")),
    /subject must be Damned Ventures LLC/,
  );
  assert.throws(
    () => assertVerification("fixture.exe", valid.replace("Timestamp: present", "")),
    /timestamp is missing/,
  );
  assert.throws(
    () => assertVerification("fixture.exe", valid.replace("Successfully verified", "Verification failed")),
    /did not report Valid/,
  );
});

test("upload uses GitHub CLI and never invokes the R2 runner", () => {
  const source = readFileSync(upload, "utf8");
  const githubSource = readFileSync(path.join(root, "github-release.mjs"), "utf8");
  assert.match(githubSource, /run\("gh", \["release", "upload"/);
  assert.doesNotMatch(source, /wrangler|CLOUDFLARE_API_TOKEN|RIGHTAPPS_API_URL/i);
  assert.doesNotMatch(source, /\bnpx\b/);
});

test("upload accepts only GitHub Releases as configured product distribution", () => {
  const source = readFileSync(upload, "utf8");
  assert.match(source, /distribution\.provider/);
  assert.match(source, /github-releases/);
  assert.match(source, /releaseConfig\?\.distribution\?\.repository/);
});
