import assert from "node:assert/strict";
import { existsSync, mkdtempSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import test from "node:test";

const release = fileURLToPath(new URL("./release.mjs", import.meta.url));

function fixture({ signed = true, publish = false, packageJson } = {}) {
  const dir = mkdtempSync(path.join(os.tmpdir(), "right-release-test-"));
  const config = path.join(dir, "right-release.config.mjs");
  writeFileSync(
    path.join(dir, "package.json"),
    JSON.stringify(
      packageJson ?? {
        name: "fixture",
        scripts: { "release:patch:win": "right-release --platform win --tier patch" },
        dependencies: { "@rightkit/license": "^0.1.5", "@rightkit/logs": "^0.1.3" },
        devDependencies: { "@rightkit/release": "^0.2.10" },
      },
      null,
      2,
    ),
  );
  writeFileSync(
    config,
    `export default ${JSON.stringify({
      schema: 1,
      app: "fixture",
      packageManager: "pnpm",
      checks: [],
      targets: {
        win: {
          signed,
          package: { cmd: "node", args: ["-e", "process.exit(0)"] },
          ...(publish ? { publish: { cmd: "node", args: ["publish-update.mjs"] } } : {}),
          artifacts: [],
          sign: { files: ["fixture.exe"] },
          updater: { artifacts: [{ file: "fixture.exe", signature: "fixture.exe.sig", platform: "windows-x86_64", key: "fixture/fixture.exe" }] },
          hardening: [],
        },
      },
    })};\n`,
  );
  return config;
}

function lockFixture() {
  const dir = mkdtempSync(path.join(os.tmpdir(), "right-release-lock-test-"));
  const config = path.join(dir, "right-release.config.mjs");
  writeFileSync(
    path.join(dir, "package.json"),
    JSON.stringify(
      {
        name: "fixture-lock",
        scripts: { "release:patch:mac": "right-release --platform mac --tier patch" },
        dependencies: { "@rightkit/license": "^0.1.5", "@rightkit/logs": "^0.1.3" },
        devDependencies: { "@rightkit/release": "^0.2.10" },
      },
      null,
      2,
    ),
  );
  writeFileSync(
    config,
    `export default ${JSON.stringify({
      schema: 1,
      app: "fixture-lock",
      packageManager: "pnpm",
      checks: [],
      targets: {
        mac: {
          signed: true,
          package: { cmd: "node", args: ["-e", "process.exit(0)"] },
          artifacts: [],
          hardening: [],
          installer: { artifacts: [] },
          updater: { artifacts: [] },
        },
      },
    })};\n`,
  );
  return { dir, config };
}

function run(config, ...args) {
  return spawnSync(process.execPath, [release, "--config", config, "--platform", "win", "--dry-run", ...args], {
    encoding: "utf8",
  });
}

function runRaw(config, ...args) {
  return spawnSync(process.execPath, [release, "--config", config, ...args], {
    encoding: "utf8",
  });
}

test("rejects a release without an explicit entitlement tier", () => {
  const result = run(fixture());
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /--tier is required.*patch\|update/i);
});

test("accepts patch and exposes it to the signed package command", () => {
  const result = run(fixture(), "--tier=patch");
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /tier=patch/);
  assert.match(result.stdout, /RIGHT_RELEASE_TIER=patch/);
});

test("accepts update as the Pro-gated feature tier", () => {
  const result = run(fixture(), "--tier", "update");
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /tier=update/);
  assert.match(result.stdout, /RIGHT_RELEASE_TIER=update/);
});

test("rejects an invalid entitlement tier", () => {
  const result = run(fixture(), "--tier=minor");
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /invalid --tier.*patch\|update/i);
});

test("rejects targets that do not explicitly declare signed release output", () => {
  const result = run(fixture({ signed: false }), "--tier=patch");
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /signed: true/);
});

test("rejects Git, path, or link RightKit app dependencies before release work starts", () => {
  const result = run(
    fixture({
      packageJson: {
        name: "fixture",
        scripts: { "release:patch:win": "node ../tools/right-release/release.mjs --tier patch" },
        dependencies: {
          "@rightkit/license": "git+https://github.com/adrdsouza/rightkit.git#main",
          "@rightkit/logs": "link:../../../rightkit/packages/logs",
        },
        devDependencies: {
          "@rightkit/release": "github:adrdsouza/claude#main&path:/tools/right-release",
        },
      },
    }),
    "--tier=patch",
  );
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /right-release bin|published npm package/i);
});

test("publish runs the signed package step before the updater publication step", () => {
  const result = run(fixture({ publish: true }), "--tier=update", "--upload");
  assert.equal(result.status, 0, result.stderr);
  const packageAt = result.stdout.indexOf("node -e process.exit(0)");
  const publishAt = result.stdout.indexOf("node publish-update.mjs");
  assert.ok(packageAt >= 0, result.stdout);
  assert.ok(publishAt > packageAt, result.stdout);
});

test("Windows patch signs the installer then minisigns the free updater payload", () => {
  const result = run(fixture({ publish: true }), "--tier=patch", "--upload");
  assert.equal(result.status, 0, result.stderr);
  const azureAt = result.stdout.indexOf("sign-windows.mjs");
  const updaterAt = result.stdout.indexOf("sign-updater.mjs");
  const publishAt = result.stdout.indexOf("publish-update.mjs");
  assert.ok(azureAt >= 0, result.stdout);
  assert.ok(updaterAt > azureAt, result.stdout);
  assert.ok(publishAt > updaterAt, result.stdout);
});

test("Windows update re-signs the updater artifact after Azure code signing", () => {
  const result = run(fixture({ publish: true }), "--tier=update", "--upload");
  assert.equal(result.status, 0, result.stderr);
  const azureAt = result.stdout.indexOf("sign-windows.mjs");
  const updaterAt = result.stdout.indexOf("sign-updater.mjs");
  const publishAt = result.stdout.indexOf("publish-update.mjs");
  assert.ok(azureAt >= 0, result.stdout);
  assert.ok(updaterAt > azureAt, result.stdout);
  assert.ok(publishAt > updaterAt, result.stdout);
});

test("release lock is single-flight and is cleaned after success", () => {
  const { dir, config } = lockFixture();
  const lockPath = path.join(dir, ".cache", "right-release", "mac.lock.json");
  const result = runRaw(config, "--platform", "mac", "--tier=patch");
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /right-release: done/);
  assert.equal(existsSync(lockPath), false);
});
