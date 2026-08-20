import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdtempSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { execFileSync } from "node:child_process";
import { canonicalAddonManifest, createAddonManifest } from "./addon-contract.mjs";
import { prepareGitHubAddonRelease, prepareGitHubRelease, publishGitHubRelease } from "./github-release.mjs";

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

function addonFixture() {
  const root = mkdtempSync(path.join(os.tmpdir(), "right-release-github-addon-"));
  mkdirSync(path.join(root, "out"));
  for (const [name, bytes] of [["membrane", "command"], ["cortex-service", "service"], ["icon.png", "icon"], ["LICENSE", "license"], ["EULA.txt", "eula"], ["PRIVACY.md", "privacy"], ["THIRD-PARTY-NOTICES.txt", "notices"]]) writeFileSync(path.join(root, "out", name), bytes);
  execFileSync("git", ["init", "--initial-branch", "main"], { cwd: root });
  execFileSync("git", ["config", "user.email", "test@example.com"], { cwd: root });
  execFileSync("git", ["config", "user.name", "Test"], { cwd: root });
  writeFileSync(path.join(root, "tracked"), "source"); execFileSync("git", ["add", "."], { cwd: root }); execFileSync("git", ["commit", "-m", "source"], { cwd: root });
  const files = [["command", "membrane", true], ["service", "cortex-service", true], ["icon", "icon.png", false], ["license", "LICENSE", false], ["eula", "EULA.txt", false], ["privacy", "PRIVACY.md", false], ["third-party-notices", "THIRD-PARTY-NOTICES.txt", false]].map(([role, name, executable]) => ({ role, name, source: `out/${name}`, executable }));
  const config = { schema: 1, kind: "headless-addon", addon: "membrane", version: "0.1.0", packageManager: "pnpm", distribution: { provider: "github-releases", repository: "Orthic-Labs/Membrane" }, checks: [], buildInputs: { include: ["tracked"] }, consumer: { contract: "orthic-product-v1" }, targets: { win: { targetTriple: "x86_64-pc-windows-msvc", build: { cmd: "false", args: [] }, signing: { contract: "azure-artifact-signing-v1" }, files } } };
  const commit = execFileSync("git", ["rev-parse", "HEAD"], { cwd: root, encoding: "utf8" }).trim();
  const manifest = createAddonManifest({ config, root, platform: "win", commit, signing: { command: { contract: "test-fixture-v1", status: "verified" }, service: { contract: "test-fixture-v1", status: "verified" } } });
  const sealed = path.join(root, ".right-release", "addons", "membrane", "0.1.0", commit.slice(0, 8), "win"); mkdirSync(sealed, { recursive: true });
  for (const file of manifest.files) writeFileSync(path.join(sealed, file.name), readFileSync(path.join(root, "out", file.name)));
  writeFileSync(path.join(sealed, "addon-manifest.json"), canonicalAddonManifest(manifest));
  return { root, config };
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

test("GitHub add-on plan uses a content-addressed platform tag and manifest URL", () => {
  const fx = addonFixture();
  const plan = prepareGitHubAddonRelease({ repoRoot: fx.root, config: fx.config, platform: "win", repo: "Orthic-Labs/Membrane" });
  assert.match(plan.tag, /^addon-membrane-v0\.1\.0-win-sha256-[0-9a-f]{64}$/);
  assert.equal(plan.assets.at(-1), plan.manifestPath);
  assert.equal(plan.manifestUrl, `https://github.com/Orthic-Labs/Membrane/releases/download/${plan.tag}/addon-manifest.json`);
});

test("existing GitHub add-on release is verified without upload mutation", () => {
  const asset = path.join(mkdtempSync(path.join(os.tmpdir(), "right-release-existing-")), "addon-manifest.json"); writeFileSync(asset, "sealed");
  const calls = [];
  const run = (_command, args, options = {}) => {
    calls.push(args);
    if (args[0] === "repo") return JSON.stringify({ visibility: "PUBLIC" });
    if (args[0] === "release" && args[1] === "view") return options.allowFailure ? { ok: true } : "";
    if (args[0] === "release" && args[1] === "download") { mkdirSync(args[args.indexOf("--dir") + 1], { recursive: true }); writeFileSync(path.join(args[args.indexOf("--dir") + 1], path.basename(asset)), "sealed"); return options.allowFailure ? { ok: true, output: "" } : ""; }
    throw new Error(`unexpected mutation: ${args.join(" ")}`);
  };
  const result = publishGitHubRelease({ kind: "addon", manifest: { files: [] }, sealedDir: path.dirname(asset), tag: "immutable", assets: [asset], manifestUrl: "https://example.test/manifest" }, { repo: "Orthic-Labs/Membrane", run });
  assert.equal(result.status, "already-verified");
  assert.equal(calls.some((args) => args[0] === "release" && args[1] === "upload"), false);
});

test("partial GitHub add-on release uploads only missing assets then verifies all", () => {
  const root = mkdtempSync(path.join(os.tmpdir(), "right-release-resume-")); const first = path.join(root, "first"); const second = path.join(root, "second"); writeFileSync(first, "one"); writeFileSync(second, "two");
  const remote = new Map([["first", "one"]]); const uploads = [];
  const run = (_command, args, options = {}) => {
    if (args[0] === "repo") return JSON.stringify({ visibility: "PUBLIC" });
    if (args[0] === "release" && args[1] === "view") return { ok: true };
    if (args[0] === "release" && args[1] === "download") { const name = args[args.indexOf("--pattern") + 1]; if (!remote.has(name)) return { ok: false, error: "no assets match" }; const dir = args[args.indexOf("--dir") + 1]; mkdirSync(dir, { recursive: true }); writeFileSync(path.join(dir, name), remote.get(name)); return { ok: true, output: "" }; }
    if (args[0] === "release" && args[1] === "upload") { for (const value of args.slice(3, args.indexOf("--repo"))) { uploads.push(path.basename(value)); remote.set(path.basename(value), readFileSync(value, "utf8")); } return ""; }
    throw new Error(`unexpected call: ${args.join(" ")}`);
  };
  const result = publishGitHubRelease({ kind: "addon", manifest: { files: [] }, sealedDir: root, tag: "immutable", assets: [first, second], manifestUrl: "https://example.test/manifest" }, { repo: "Orthic-Labs/Membrane", run });
  assert.equal(result.status, "resumed-verified"); assert.deepEqual(uploads, ["second"]);
});
