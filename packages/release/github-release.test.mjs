import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdtempSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { prepareGitHubRelease, publishGitHubRelease } from "./github-release.mjs";

const sha256 = (value) => createHash("sha256").update(value).digest("hex");

function fixture() {
  const root = mkdtempSync(path.join(os.tmpdir(), "right-release-github-"));
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

test("GitHub plan derives tag, notes, installer, manifest & checksums only from sealed bytes", () => {
  const fx = fixture();
  const plan = prepareGitHubRelease({ repoRoot: fx.root, releaseId: fx.releaseId, platform: "win", repo: "Orthic-Labs/CutRight" });
  assert.equal(plan.tag, "v1.2.3");
  assert.equal(plan.assets.length, 3);
  assert.match(plan.notes, /abcdef1234567890/);
  assert.equal(readFileSync(plan.assets[2], "utf8"), `${sha256("signed-installer")}  CutRight.exe\n`);
});

test("dry-run checks public visibility without creating or uploading a release", () => {
  const fx = fixture();
  const plan = prepareGitHubRelease({ repoRoot: fx.root, releaseId: fx.releaseId, platform: "win", repo: "Orthic-Labs/CutRight" });
  const calls = [];
  const run = (_command, args, options = {}) => {
    calls.push(args);
    if (args[0] === "repo") return JSON.stringify({ visibility: "PUBLIC" });
    if (args[0] === "release" && args[1] === "view") return options.allowFailure ? { ok: false } : "";
    throw new Error(`unexpected mutation: ${args.join(" ")}`);
  };
  assert.equal(publishGitHubRelease(plan, { repo: "Orthic-Labs/CutRight", dryRun: true, run }).status, "would-create");
  assert.equal(calls.length, 2);
});
