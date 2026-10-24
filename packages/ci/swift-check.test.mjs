import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { buildRenderContext, validateManifest } from "./manifest.mjs";
import { renderLanes } from "./sync.mjs";
import { renderTemplate } from "./render.mjs";

const CHECK = { runner: "macos-15", xcodeVersion: "16.4", projectDirectory: "ios", spec: "project.yml", project: "Widgets.xcodeproj", scheme: "Widgets", xcodegenVersion: "2.44.1" };
const BASE = { schemaVersion: 1, compile: "github-actions-only", lanes: ["swift-check"], matrix: { os: ["macos-15"], node: ["26.8.1"] }, packageManager: "pnpm@11.25.0", templateVersion: "1.8.0", swift: { check: CHECK } };
const render = (manifest = BASE) => renderLanes(manifest, "/repos/widgets")[0].content;

test("swift-check supports a standalone default-profile manifest with or without private packages", () => {
  assert.deepEqual(validateManifest(BASE).errors, []);
  assert.deepEqual(validateManifest({ ...BASE, swift: { check: CHECK, privatePackages: [{ host: "github.com", secret: "SWIFT_READ_TOKEN" }] } }).errors, []);
  assert.equal(renderLanes(BASE, "/repos/widgets")[0].filename, "swift-check.yml");
  assert.match(render(), /^name: swift-check$/m);
});

test("swift-check rejects missing configuration, unpinned tools, unsafe fields and unknown keys", () => {
  assert.equal(validateManifest({ ...BASE, swift: undefined }).valid, false);
  assert.equal(validateManifest({ ...BASE, swift: { check: null } }).valid, false);
  assert.equal(validateManifest({ ...BASE, lanes: ["ci"] }).valid, false);
  for (const [key, values] of Object.entries({
    runner: ["macos-latest", "self-hosted", "xcode-27", "ubuntu-24.04", "${{ secrets.TOKEN }}"],
    xcodeVersion: ["latest", "16", "16.4;id", "16.4\n"],
    xcodegenVersion: ["latest", "2.44", "2.44.1$(id)"],
    projectDirectory: ["../ios", "/tmp/ios", "ios dir", "ios\\x", "${{ github.ref }}"],
    spec: ["../project.yml", "-x.yml", "project.yml\nrun: id", "project.json"],
    project: ["../App.xcodeproj", "App.xcworkspace", "$(id).xcodeproj"],
    scheme: ["-scheme", "App Name", "App'", "${{ secrets.TOKEN }}"],
    arbitrary: ["value"],
  })) for (const value of values) {
    assert.equal(validateManifest({ ...BASE, swift: { check: { ...CHECK, [key]: value } } }).valid, false, `${key}=${value}`);
  }
});

test("swift-check pins actions, tools and read-only permissions; test execution stays unsigned Debug", () => {
  const out = render();
  for (const line of out.split("\n").filter((line) => line.includes("uses:"))) assert.match(line, /uses: [^@]+@[a-f0-9]{40}(?: |$)/);
  assert.match(out, /^permissions:\n  contents: read$/m);
  assert.doesNotMatch(out, /: write|write-all|workflow_dispatch|pull_request_target|workflow_run|signing|notary|\bsecrets\./);
  assert.match(out, /runs-on: macos-15/);
  assert.match(out, /DEVELOPER_DIR: \/Applications\/Xcode_16\.4\.app\/Contents\/Developer/);
  assert.match(out, /test "\$\(xcodegen --version\)" = 'Version: 2\.44\.1'/);
  assert.match(out, /persist-credentials: false/);
  assert.match(out, /xcodegen generate --spec 'project.yml'/);
  assert.match(out, /simctl list devices available --json/);
  assert.match(out, /xcodebuild test -project 'Widgets.xcodeproj' -scheme 'Widgets'/);
  assert.match(out, /-configuration Debug -destination "platform=iOS Simulator,id=\$RIGHT_GIT_SIMULATOR_UDID"/);
  assert.match(out, /-scmProvider system[\s\S]*CODE_SIGNING_ALLOWED=NO/);
  assert.match(out, /Upload XCTest results\n        if: \$\{\{ always\(\) \}\}/);
  assert.match(out, /right-release ownership --app \./);
});

test("swift-check shares exact private-package steps and keeps cleanup before artifact upload", () => {
  const out = render({ ...BASE, swift: { check: CHECK, privatePackages: [{ host: "github.com", secret: "SWIFT_READ_TOKEN" }] } });
  const configure = out.indexOf("Configure private Swift package access");
  const generation = out.indexOf("Generate Xcode project");
  const execution = out.indexOf("Run Debug XCTest");
  const cleanup = out.indexOf("Remove private Swift package access");
  assert.ok(configure < generation && generation < execution && execution < cleanup && cleanup < out.indexOf("Upload XCTest results"));
  assert.match(out, /test -n "\$RIGHT_GIT_PRIVATE_TOKEN_0"[\s\S]*secret SWIFT_READ_TOKEN is unavailable/);
  assert.match(out, /Remove private Swift package access\n        if: \$\{\{ always\(\) \}\}/);
  assert.equal((out.match(/secrets\.SWIFT_READ_TOKEN/g) ?? []).length, 2);
  for (const line of out.split("\n").filter((line) => line.includes("secrets."))) assert.match(line, /^ {10}RIGHT_GIT_PRIVATE_TOKEN_0:/);
});

test("swift-check config does not change existing lane rendering when lane is unset", () => {
  const manifest = { ...BASE, lanes: ["ci", "qualification", "package-smoke"], swift: undefined };
  const before = renderLanes(manifest, "/repos/widgets");
  const after = renderLanes({ ...manifest, swift: { check: CHECK } }, "/repos/widgets");
  assert.deepEqual(after, before);
});

test("swift-check render refuses credential-bearing unsafe triggers", () => {
  const manifest = { ...BASE, swift: { check: CHECK, privatePackages: [{ host: "github.com", secret: "SWIFT_READ_TOKEN" }] } };
  for (const trigger of ["pull_request_target", "workflow_run"]) {
    assert.throws(() => renderTemplate(render(manifest).replace("  pull_request:", `  ${trigger}:`), buildRenderContext(manifest, "/repos/widgets", "swift-check")), /refusing to render/);
  }
});

test("swift-check generated workflow passes actionlint", (t) => {
  const available = spawnSync("actionlint", ["-version"], { encoding: "utf8" });
  if (available.error?.code === "ENOENT") return t.skip("actionlint is unavailable");
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "right-git-swift-check-"));
  try {
    const filename = path.join(dir, "swift-check.yml");
    fs.writeFileSync(filename, render({ ...BASE, swift: { check: CHECK, privatePackages: [{ host: "github.com", secret: "SWIFT_READ_TOKEN" }] } }));
    const result = spawnSync("actionlint", ["-shellcheck=", "-pyflakes=", filename], { encoding: "utf8" });
    assert.equal(result.status, 0, result.stdout + result.stderr);
  } finally { fs.rmSync(dir, { recursive: true, force: true }); }
});
