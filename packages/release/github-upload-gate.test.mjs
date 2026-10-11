import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { prepareGitHubRelease, publishGitHubRelease } from "./github-release.mjs";
import { assertUploadTier, authorizeGitHubUpload, resolveGitHubDistribution } from "./github-upload-gate.mjs";

const sha256 = (value) => createHash("sha256").update(value).digest("hex");
const GITHUB = { distribution: { provider: "github-releases", repository: "Orthic-Labs/CutRight" } };

function sealedFixture() {
  const root = mkdtempSync(path.join(os.tmpdir(), "right-release-gate-"));
  const releaseId = "cutright-1.2.3-abcdef12";
  const sealedDir = path.join(root, ".right-release", "sealed", releaseId, "windows");
  mkdirSync(sealedDir, { recursive: true });
  writeFileSync(path.join(sealedDir, "CutRight.exe"), "signed-installer");
  writeFileSync(path.join(sealedDir, "release-manifest.json"), `${JSON.stringify({
    schema: 1, releaseId, app: "cutright", version: "1.2.3", commit: "abcdef1234567890", platform: "win",
    files: [{ role: "installer", name: "CutRight.exe", sha256: sha256("signed-installer"), sizeBytes: 16 }],
    checkpoints: ["sealed"],
  })}\n`);
  return { root, releaseId };
}

test("only an explicit patch or update tier is accepted", () => {
  assert.equal(assertUploadTier("patch"), "patch");
  assert.equal(assertUploadTier("update"), "update");
  for (const tier of [undefined, null, "", "Patch", "patch ", "minor", "major", "latest", "r2", 1]) {
    assert.throws(() => assertUploadTier(tier), /tier is required \(patch\|update\)/, `accepted ${JSON.stringify(tier)}`);
  }
});

test("a release config without the github-releases provider is refused", () => {
  for (const config of [
    undefined,
    null,
    {},
    { distribution: {} },
    { distribution: { repository: "Orthic-Labs/CutRight" } },
    { distribution: { provider: "r2-rightapps", repository: "Orthic-Labs/CutRight" } },
    { distribution: { provider: "r2", repository: "Orthic-Labs/CutRight" } },
    { distribution: { provider: "GitHub-Releases", repository: "Orthic-Labs/CutRight" } },
    { distribution: { provider: "github", repository: "Orthic-Labs/CutRight" } },
  ]) {
    assert.throws(() => resolveGitHubDistribution(config), /distribution\.provider must be github-releases/, JSON.stringify(config));
  }
});

test("github-releases needs a configured repository, and --repo may only restate it", () => {
  assert.throws(() => resolveGitHubDistribution({ distribution: { provider: "github-releases" } }), /distribution\.repository is required for github-releases/);
  assert.throws(() => resolveGitHubDistribution({ distribution: { provider: "github-releases", repository: "" } }, "Orthic-Labs/CutRight"), /distribution\.repository is required for github-releases/);
  assert.throws(() => resolveGitHubDistribution(GITHUB, "Someone-Else/Elsewhere"), /--repo must match configured distribution\.repository \(Orthic-Labs\/CutRight\)/);
  assert.equal(resolveGitHubDistribution(GITHUB), "Orthic-Labs/CutRight");
  assert.equal(resolveGitHubDistribution(GITHUB, ""), "Orthic-Labs/CutRight");
  assert.equal(resolveGitHubDistribution(GITHUB, "Orthic-Labs/CutRight"), "Orthic-Labs/CutRight");
});

test("authorization refuses a missing tier, release or platform before it touches git", async () => {
  const cwd = mkdtempSync(path.join(os.tmpdir(), "right-release-gate-cwd-"));
  const base = { tier: "patch", releaseId: "cutright-1.2.3-abcdef12", platform: "win", cwd };
  await assert.rejects(authorizeGitHubUpload({ ...base, tier: undefined }), /^Error: tier is required \(patch\|update\)$/);
  await assert.rejects(authorizeGitHubUpload({ ...base, tier: "minor" }), /^Error: tier is required \(patch\|update\)$/);
  await assert.rejects(authorizeGitHubUpload({ ...base, releaseId: "" }), /^Error: --release is required$/);
  await assert.rejects(authorizeGitHubUpload({ ...base, platform: "linux" }), /^Error: --platform must be win or mac$/);
});

test("a tiered request outside a repository stops at the first local check and never reaches GitHub", async () => {
  const cwd = mkdtempSync(path.join(os.tmpdir(), "right-release-gate-cwd-"));
  await assert.rejects(
    authorizeGitHubUpload({ tier: "update", releaseId: "cutright-1.2.3-abcdef12", platform: "win", cwd, repo: "Someone-Else/Elsewhere" }),
    (error) => error instanceof Error && !/tier is required/.test(error.message),
  );
});

test("refused configs never reach the network, and an accepted one reaches it only through the injected runner", () => {
  const fx = sealedFixture();
  const attempt = (config, requestedRepo) => {
    const repo = resolveGitHubDistribution(config, requestedRepo);
    const plan = prepareGitHubRelease({ repoRoot: fx.root, releaseId: fx.releaseId, platform: "win", repo });
    const calls = [];
    const run = (_command, args, options = {}) => {
      calls.push(args);
      if (args[0] === "repo") return JSON.stringify({ visibility: "PUBLIC" });
      if (args[0] === "release" && args[1] === "view") return options.allowFailure ? { ok: false } : "";
      throw new Error(`unexpected mutation: ${args.join(" ")}`);
    };
    return { result: publishGitHubRelease(plan, { repo, dryRun: true, run }), calls };
  };
  assert.throws(() => attempt({ distribution: { provider: "r2-rightapps", repository: "Orthic-Labs/CutRight" } }, ""), /distribution\.provider must be github-releases/);
  assert.throws(() => attempt(GITHUB, "Someone-Else/Elsewhere"), /--repo must match/);
  const accepted = attempt(GITHUB, "");
  assert.equal(accepted.result.status, "would-create");
  assert.deepEqual(accepted.calls.map((args) => args.slice(0, 2).join(" ")), ["repo view", "release view"]);
});
