import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import test from "node:test";

const publisher = fileURLToPath(new URL("./publish-update.mjs", import.meta.url));

function fixture({ materialize = false } = {}) {
  const dir = mkdtempSync(path.join(os.tmpdir(), "right-publish-test-"));
  const config = path.join(dir, "right-release.config.mjs");
  writeFileSync(config, `export default ${JSON.stringify({
    schema: 1,
    app: "fixture",
    version: "1.2.3",
    distribution: { provider: "r2-rightapps" },
    targets: {
      win: {
        installer: { artifacts: [{ file: "Fixture-Setup.exe", key: "fixture/installers/windows/current/Fixture-Setup.exe" }] },
        updater: {
          artifacts: [
            { file: "Fixture.exe", signature: "Fixture.exe.sig", platform: "windows-x86_64", key: "fixture/updates/windows/current/Fixture.exe" },
            { file: "Fixture.exe", signature: "Fixture.exe.sig", platform: "windows-arm64", key: "fixture/updates/windows/current/Fixture.exe" },
          ],
        },
      },
    },
  })};\n`);
  if (materialize) {
    writeFileSync(path.join(dir, "Fixture-Setup.exe"), "signed installer");
    writeFileSync(path.join(dir, "Fixture.exe"), "signed updater");
    writeFileSync(path.join(dir, "Fixture.exe.sig"), "updater signature");
  }
  return config;
}

function versionedFixture() {
  const dir = mkdtempSync(path.join(os.tmpdir(), "right-publish-test-"));
  const config = path.join(dir, "right-release.config.mjs");
  writeFileSync(config, `export default ${JSON.stringify({
    schema: 1,
    app: "fixture",
    version: "1.2.3",
    distribution: { provider: "r2-rightapps" },
    targets: {
      win: {
        installer: { artifacts: [{ file: "Fixture-Setup.exe", key: "fixture/installers/windows/1.2.3/Fixture-Setup.exe" }] },
        updater: {
          artifacts: [
            { file: "Fixture.exe", signature: "Fixture.exe.sig", platform: "windows-x86_64", key: "fixture/1.2.3/windows/Fixture.exe" },
          ],
        },
      },
    },
  })};\n`);
  return config;
}

function installerOnlyFixture() {
  const dir = mkdtempSync(path.join(os.tmpdir(), "right-publish-test-"));
  const config = path.join(dir, "right-release.config.mjs");
  writeFileSync(config, `export default ${JSON.stringify({
    schema: 1,
    app: "fixture",
    version: "1.2.3",
    distribution: { provider: "r2-rightapps" },
    targets: {
      win: {
        installer: { artifacts: [{ file: "Fixture-Setup.exe", key: "fixture/installers/windows/current/Fixture-Setup.exe" }] },
        updater: { artifacts: [] },
      },
    },
  })};\n`);
  return config;
}

function run(tier) {
  const config = fixture();
  return spawnSync(process.execPath, [publisher, "--config", config, "--platform", "win", "--dry-run"], {
    encoding: "utf8",
    env: { ...process.env, RIGHT_RELEASE_TIER: tier },
  });
}

function registrationBody(result) {
  return result.stdout
    .split(/\r?\n/)
    .filter((line) => line.startsWith('{'))
    .map((line) => JSON.parse(line))
    .find((body) => body.artifactManifest);
}

test("patch registers a free updater manifest backed by public installer-path objects", () => {
  const result = run("patch");
  assert.equal(result.status, 0, result.stderr);
  const patchPayloadUpload = result.stdout.indexOf("UPLOAD public/fixture/installers/windows/current/Fixture.exe");
  const installerUpload = result.stdout.indexOf("UPLOAD public/fixture/installers/windows/current/Fixture-Setup.exe");
  const registration = result.stdout.indexOf("/v1/admin/apps/patches");
  assert.ok(patchPayloadUpload >= 0, "patch updater payload must upload to the public installer path");
  assert.ok(installerUpload >= 0, "patch download installer must upload to the public installer path");
  assert.ok(patchPayloadUpload < installerUpload, "patch should upload app-update payloads before replacing the public download installer");
  assert.ok(installerUpload < registration, "patch should register only after uploads complete");
  assert.doesNotMatch(result.stdout, /UPLOAD public\/fixture\/updates\//);
  assert.doesNotMatch(result.stdout, /UPLOAD private\//);
  assert.match(result.stdout, /pub-6c73208d46c245a9b4881d5e02f6b618\.r2\.dev\/fixture\/installers\/windows\/current\/Fixture\.exe/);
  assert.match(result.stdout, /POST .*\/v1\/admin\/apps\/patches/);
  assert.match(result.stdout, /"tier":"patch"/);
  assert.match(result.stdout, /"platform":"windows"/);
  assert.doesNotMatch(result.stdout, /UPLOAD .*\.sig/);
  assert.equal((result.stdout.match(/UPLOAD /g) ?? []).length, 2, "patch may upload only one installer and one updater object per OS");
  assert.match(result.stdout, /PRUNE rightapps-downloads\/fixture\/windows/);
  assert.match(result.stdout, /PRUNE rightapps-updates\/fixture\/windows keep=<none>/);
  const body = registrationBody(result);
  assert.deepEqual(
    {
      schema: body.artifactManifest.schema,
      appKey: body.artifactManifest.appKey,
      appVersion: body.artifactManifest.appVersion,
      tier: body.artifactManifest.tier,
      platform: body.artifactManifest.platform,
    },
    { schema: 1, appKey: 'fixture', appVersion: '1.2.3', tier: 'patch', platform: 'windows' },
  );
  assert.match(body.artifactManifest.pipelineVersion, /^\d+\.\d+\.\d+$/);
  assert.deepEqual(body.artifactManifest.artifacts.map((artifact) => [artifact.kind, artifact.r2Key]), [
    ['installer', 'fixture/installers/windows/current/Fixture-Setup.exe'],
    ['updater', 'fixture/installers/windows/current/Fixture.exe'],
  ]);
  assert.match(body.artifactManifest.artifacts[1].updaterSignature, /^<signature:/);
});

test("routes feature updates to the Pro-gated release endpoint", () => {
  const result = run("update");
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /POST .*\/v1\/admin\/apps\/releases/);
  assert.match(result.stdout, /"tier":"update"/);
  assert.match(result.stdout, /"platform":"windows"/);
  assert.match(result.stdout, /UPLOAD private\/fixture\/updates\/windows\/current\/Fixture\.exe/);
  assert.doesNotMatch(result.stdout, /UPLOAD .*\.sig/);
  assert.equal((result.stdout.match(/UPLOAD /g) ?? []).length, 1, "feature update may upload only one updater object per OS");
  assert.match(result.stdout, /PRUNE rightapps-downloads\/fixture\/windows/);
  assert.match(result.stdout, /PRUNE rightapps-updates\/fixture\/windows/);
  assert.doesNotMatch(result.stdout, /UPLOAD .*Fixture-Setup\.exe/);
  const body = registrationBody(result);
  assert.equal(body.artifactManifest.tier, 'update');
  assert.deepEqual(body.artifactManifest.artifacts.map((artifact) => [artifact.kind, artifact.r2Key]), [
    ['updater', 'fixture/updates/windows/current/Fixture.exe'],
  ]);
});

test("allows an installer-only patch without registering an updater manifest", () => {
  const config = installerOnlyFixture();
  const result = spawnSync(process.execPath, [publisher, "--config", config, "--platform", "win", "--dry-run"], {
    encoding: "utf8",
    env: { ...process.env, RIGHT_RELEASE_TIER: "patch" },
  });
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /UPLOAD public\/fixture\/installers\/windows\/current\/Fixture-Setup\.exe/);
  assert.match(result.stdout, /NO patch updater artifacts/);
  assert.doesNotMatch(result.stdout, /\/v1\/admin\/apps\/patches/);
  assert.doesNotMatch(result.stdout, /UPLOAD private\//);
  assert.match(result.stdout, /PRUNE rightapps-downloads\/fixture\/windows/);
});

test("refuses to publish without a valid tier", () => {
  const result = run("");
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /tier is required.*patch\|update/);
});

test("accepts portable --tier argument", () => {
  const config = installerOnlyFixture();
  const result = spawnSync(process.execPath, [publisher, "--config", config, "--platform", "win", "--tier", "patch", "--dry-run"], {
    encoding: "utf8",
    env: { ...process.env, RIGHT_RELEASE_TIER: "" },
  });
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /UPLOAD public\/fixture\/installers\/windows\/current\/Fixture-Setup\.exe/);
});

test("refuses GitHub distribution before R2 publication", () => {
  const config = fixture();
  writeFileSync(config, `export default ${JSON.stringify({
    schema: 1,
    app: "fixture",
    version: "1.2.3",
    distribution: { provider: "github-releases", repository: "Orthic-Labs/Fixture" },
    targets: { win: { installer: { artifacts: [] }, updater: { artifacts: [] } } },
  })};\n`);
  const result = spawnSync(process.execPath, [publisher, "--config", config, "--platform", "win", "--tier", "patch", "--dry-run"], { encoding: "utf8" });
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /distribution\.provider must be r2-rightapps/);
});

test("refuses versioned R2 object keys that would accumulate old releases", () => {
  const config = versionedFixture();
  const result = spawnSync(process.execPath, [publisher, "--config", config, "--platform", "win", "--dry-run"], {
    encoding: "utf8",
    env: { ...process.env, RIGHT_RELEASE_TIER: "patch" },
  });
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /current R2 object/);
});

test("refuses versioned updater object keys for gated feature updates", () => {
  const config = versionedFixture();
  const result = spawnSync(process.execPath, [publisher, "--config", config, "--platform", "win", "--dry-run"], {
    encoding: "utf8",
    env: { ...process.env, RIGHT_RELEASE_TIER: "update" },
  });
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /current R2 object/);
});

test("requires the durable release credential before the first patch upload", () => {
  const config = fixture({ materialize: true });
  const env = {
    ...process.env,
    APPDATA: path.join(path.dirname(config), "empty-appdata"),
    RIGHT_RELEASE_TIER: "patch",
    RIGHTAPPS_RELEASE_TOKEN_FILE: path.join(path.dirname(config), "missing-release-token"),
    RIGHTAPPS_ADMIN_TOKEN: "expired-admin-session",
  };
  delete env.RIGHTAPPS_RELEASE_TOKEN;
  const result = spawnSync(process.execPath, [publisher, "--config", config, "--platform", "win"], {
    encoding: "utf8",
    env,
  });
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /RIGHTAPPS_RELEASE_TOKEN/);
  assert.doesNotMatch(result.stdout, /UPLOAD /, "credential preflight must run before R2 mutation");
});
