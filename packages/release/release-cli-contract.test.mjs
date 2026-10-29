import "./test-support/broker-guard.mjs";
import { tempRoot } from "./test-support/temp-roots.mjs";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";
import { fileURLToPath } from "node:url";
import vm from "node:vm";
import test from "node:test";

const root = path.dirname(fileURLToPath(import.meta.url));
const cli = path.join(root, "cli", "right-release.mjs");
const build = path.join(root, "build-release.mjs");
const upload = path.join(root, "upload-release.mjs");
const githubRelease = path.join(root, "github-release.mjs");
const uploadGate = path.join(root, "github-upload-gate.mjs");
const publishUpdate = path.join(root, "publish-update.mjs");
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
  // Anchor to the actual `publish` routing branch, not the earlier
  // `assertReleaseExecutionAllowed` gate condition (also spelled
  // `first === "publish"`, a few lines above `run("release.mjs", ...)` for
  // the unrelated `--version` branch) — a loose match on the bare condition
  // text produced a false positive there.
  assert.doesNotMatch(source, /\}\s*else if \(first === "publish"\) \{[\s\S]{0,500}run\("release\.mjs"/);
});

test("upload dry-run cannot write a provider-bound verified marker", () => {
  const source = readFileSync(upload, "utf8");
  assert.match(source, /if \(!dryRun\)\s+writeJson\(verifiedMarker,/);
  assert.match(source, /verified-\$\{tier\}-github-releases\.json/);
});

test("upload replay never rewrites publication state from a local verified receipt", () => {
  const source = readFileSync(upload, "utf8");
  const replay = source.match(/if \(existsSync\(verifiedMarker\)\) \{([\s\S]*?)\n\}/)?.[1] ?? "";
  assert.doesNotMatch(replay, /recordPublishedVersion/);
  assert.match(source, /if \(!dryRun\) recordPublishedVersion\([^\n]+\);/);
});

test("build is tier-neutral and rejects a tier before touching a repository", () => {
  const result = spawnSync(process.execPath, [build, "--platform", "win", "--tier", "patch"], {
    cwd: tempRoot("right-build-cli-"),
    encoding: "utf8",
  });
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /build is tier-neutral/i);
});

test("build enforces primary-checkout and build-input cleanliness guards", () => {
  const source = readFileSync(build, "utf8");
  // The primary-worktree and clean-source checks are release-source-identity.mjs assertReleaseSource (R1 unchanged without a door split).
  assert.match(source, /assertReleaseSource\(releaseSource, \{ commandId: "right-release build", skipCleanCheck: true \}\)/);
  assert.match(source, /dirtyBuildInputs\(repoRoot,/);
  assert.match(source, /dirty files can change the packaged app/i);
});

test("upload enforces the same primary-checkout vault", () => {
  // The checks moved into the gate that `right-release github --release` also runs; upload must still call it.
  const source = readFileSync(upload, "utf8");
  const gate = readFileSync(uploadGate, "utf8");
  assert.match(source, /await authorizeGitHubUpload\(/);
  assert.match(gate, /assertPrimaryReleaseCheckout\(repoRoot\)/);
  assert.match(gate, /assertCleanSource\(/);
  assert.match(gate, /sealed release was built from a different commit than the current checkout/);
  assert.match(gate, /verifyOwnershipEvidence\(/);
});

test("upload requires an explicit patch or update tier before reading a release", () => {
  const result = spawnSync(process.execPath, [upload, "--platform", "win", "--release", "fixture-1.0.0-deadbeef"], {
    cwd: tempRoot("right-upload-cli-"),
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

test("upload accepts only explicit GitHub Releases product distribution", () => {
  // The provider and repository rules live in the shared gate; neither entry point may restate or bypass them.
  const gate = readFileSync(uploadGate, "utf8");
  const source = readFileSync(upload, "utf8");
  assert.match(gate, /distribution\?\.provider !== "github-releases"/);
  assert.match(gate, /distribution\.repository is required for github-releases/);
  assert.match(gate, /--repo must match configured distribution\.repository/);
  assert.doesNotMatch(gate, /repositoryFromRemote/);
  assert.doesNotMatch(source, /repositoryFromRemote/);
  assert.doesNotMatch(source, /distribution\?\.provider/);
});

test("github --release requires an explicit patch or update tier before reading a release", () => {
  for (const tierArgs of [[], ["--tier", "minor"], ["--tier="], ["--tier", ""]]) {
    const result = spawnSync(process.execPath, [githubRelease, "--release", "fixture-1.0.0-deadbeef", "--platform", "win", ...tierArgs], {
      cwd: tempRoot("right-github-cli-"),
      encoding: "utf8",
    });
    assert.notEqual(result.status, 0, `accepted ${JSON.stringify(tierArgs)}`);
    assert.match(result.stderr, /right-release github: tier is required.*patch\|update/i);
  }
});

test("github --release with a tier passes the tier gate and is stopped by a later local check, not by the network", () => {
  const result = spawnSync(process.execPath, [githubRelease, "--release", "fixture-1.0.0-deadbeef", "--platform", "win", "--tier=patch", "--dry-run"], {
    cwd: tempRoot("right-github-cli-"),
    encoding: "utf8",
  });
  assert.notEqual(result.status, 0);
  assert.doesNotMatch(result.stderr, /tier is required/);
  assert.match(result.stderr, /^right-release github: /m);
});

test("github --release authorizes through the upload gate before it plans or contacts GitHub", () => {
  const source = readFileSync(githubRelease, "utf8");
  const main = source.slice(source.indexOf("const realOrSelf"));
  assert.match(main, /import\("\.\/github-upload-gate\.mjs"\)/);
  assert.match(main, /authorizeGitHubUpload\(\{ command: "right-release github", tier,/);
  assert.match(main, /authorizeGitHubUpload\([\s\S]*prepareGitHubRelease\([\s\S]*publishGitHubRelease\(plan, options\)/);
  assert.doesNotMatch(main, /\|\|= repositoryFromRemote/);
  // The library entry points stay policy-free; only the command-line main block carries the gate.
  assert.doesNotMatch(source.slice(0, source.indexOf("const realOrSelf")), /authorizeGitHubUpload/);
});

test("github --addon refuses --tier and --config instead of ignoring them", () => {
  const result = spawnSync(process.execPath, [githubRelease, "--addon", "membrane.config.mjs", "--platform", "win", "--tier", "patch"], {
    cwd: tempRoot("right-github-cli-"),
    encoding: "utf8",
  });
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /--tier and --config apply to --release only/);
});

test("publish-update accepts only explicit r2-rightapps or github-releases distribution", () => {
  const source = readFileSync(publishUpdate, "utf8");
  assert.match(source, /provider !== "r2-rightapps" && provider !== "github-releases"/);
  assert.match(source, /distribution\.provider must be r2-rightapps or github-releases/);
  // One signing path: the github lane reuses the sealed signatures and builds no signer or key of its own.
  assert.doesNotMatch(source, /tauri signer|TAURI_SIGNING_PRIVATE_KEY/);
});
