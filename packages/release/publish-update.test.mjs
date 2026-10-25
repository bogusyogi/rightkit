import assert from "node:assert/strict";
import { generateKeyPairSync, sign as cryptoSign } from "node:crypto";
import { mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";
import { fileURLToPath } from "node:url";
import test from "node:test";
import { runInNewContext } from "node:vm";
import { unmanagedEnv } from "./test-support/unmanaged-env.mjs";
import { RIGHT_SUITE_LEGACY_HOSTS, RIGHT_SUITE_UPDATER_SIGNING_KEY_ID } from "./suite-api.generated.mjs";
import { assertUpdaterKeyId, assertUpdaterSignature, resolveUpdaterTrust } from "./github-updater-release.mjs";

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
    env: unmanagedEnv(process.env, { RIGHT_RELEASE_TIER: tier }),
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
    env: unmanagedEnv(process.env, { RIGHT_RELEASE_TIER: "patch" }),
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
    env: unmanagedEnv(process.env, { RIGHT_RELEASE_TIER: "" }),
  });
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /UPLOAD public\/fixture\/installers\/windows\/current\/Fixture-Setup\.exe/);
});

test("refuses an unknown distribution provider", () => {
  const config = fixture();
  writeFileSync(config, `export default ${JSON.stringify({
    schema: 1,
    app: "fixture",
    version: "1.2.3",
    distribution: { provider: "s3" },
    targets: { win: { installer: { artifacts: [] }, updater: { artifacts: [] } } },
  })};
`);
  const result = spawnSync(process.execPath, [publisher, "--config", config, "--platform", "win", "--tier", "patch", "--dry-run"], { encoding: "utf8" });
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /distribution\.provider must be r2-rightapps or github-releases/);
});

function githubFixture(repository = "Orthic-Labs/Fixture") {
  const dir = mkdtempSync(path.join(os.tmpdir(), "right-publish-github-"));
  const config = path.join(dir, "right-release.config.mjs");
  writeFileSync(config, `export default ${JSON.stringify({
    schema: 1,
    app: "fixture",
    version: "1.2.3",
    distribution: { provider: "github-releases", repository },
    targets: {
      win: {
        updater: {
          artifacts: [
            { file: "Fixture_1.2.3_x64-setup.exe", signature: "Fixture_1.2.3_x64-setup.exe.sig", platform: "windows-x86_64" },
          ],
        },
      },
      mac: {
        updater: {
          artifacts: [
            { file: "Fixture.app.tar.gz", signature: "Fixture.app.tar.gz.sig", platform: "darwin-aarch64" },
          ],
        },
      },
    },
  })};
`);
  return config;
}

test("github-releases dry run plans bundle, signature and latest.json uploads with no network or key", () => {
  const result = spawnSync(process.execPath, [publisher, "--config", githubFixture(), "--platform", "win", "--tier", "update", "--dry-run"], { encoding: "utf8" });
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stdout, /UPLOAD Orthic-Labs\/Fixture@v1\.2\.3\/Fixture_1\.2\.3_x64-setup\.exe\b/);
  assert.match(result.stdout, /UPLOAD Orthic-Labs\/Fixture@v1\.2\.3\/Fixture_1\.2\.3_x64-setup\.exe\.sig/);
  assert.match(result.stdout, /UPLOAD Orthic-Labs\/Fixture@v1\.2\.3\/latest\.json/);
  assert.match(result.stdout, /"url":"https:\/\/github\.com\/Orthic-Labs\/Fixture\/releases\/download\/v1\.2\.3\/Fixture_1\.2\.3_x64-setup\.exe"/);
  assert.match(result.stdout, /github-releases would-publish v1\.2\.3/);
  for (const legacy of RIGHT_SUITE_LEGACY_HOSTS) assert.doesNotMatch(result.stdout, new RegExp(legacy.replaceAll(".", "\\.")));
});

test("github-releases refuses a missing or malformed repository", () => {
  const result = spawnSync(process.execPath, [publisher, "--config", githubFixture("not a repo"), "--platform", "win", "--tier", "update", "--dry-run"], { encoding: "utf8" });
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /distribution\.repository must be owner\/repo/);
});

test("github-releases refuses a platform that is not a Tauri updater target", () => {
  const config = githubFixture();
  writeFileSync(config, readFileSync(config, "utf8").replace("windows-x86_64", "windows-x64"));
  const result = spawnSync(process.execPath, [publisher, "--config", config, "--platform", "win", "--tier", "update", "--dry-run"], { encoding: "utf8" });
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /PUBLICATION_UPDATER_PLATFORM_INVALID/);
});

test("refuses versioned R2 object keys that would accumulate old releases", () => {
  const config = versionedFixture();
  const result = spawnSync(process.execPath, [publisher, "--config", config, "--platform", "win", "--dry-run"], {
    encoding: "utf8",
    env: unmanagedEnv(process.env, { RIGHT_RELEASE_TIER: "patch" }),
  });
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /current R2 object/);
});

test("refuses versioned updater object keys for gated feature updates", () => {
  const config = versionedFixture();
  const result = spawnSync(process.execPath, [publisher, "--config", config, "--platform", "win", "--dry-run"], {
    encoding: "utf8",
    env: unmanagedEnv(process.env, { RIGHT_RELEASE_TIER: "update" }),
  });
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /current R2 object/);
});

test("requires a seal before credentials or the first patch upload", () => {
  const config = fixture({ materialize: true });
  const env = unmanagedEnv(process.env, {
    APPDATA: path.join(path.dirname(config), "empty-appdata"),
    RIGHT_RELEASE_TIER: "patch",
    RIGHTAPPS_RELEASE_TOKEN_FILE: path.join(path.dirname(config), "missing-release-token"),
    RIGHTAPPS_ADMIN_TOKEN: "expired-admin-session",
  });
  delete env.RIGHTAPPS_RELEASE_TOKEN;
  const result = spawnSync(process.execPath, [publisher, "--config", config, "--platform", "win"], {
    encoding: "utf8",
    env,
  });
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /PUBLICATION_SEAL_REQUIRED/);
  assert.doesNotMatch(result.stdout, /UPLOAD /, "credential preflight must run before R2 mutation");
});

test("patch uploads the versioned MSIX install source and refuses a malformed versioned key", () => {
  const mk = (versionedKey) => {
    const dir = mkdtempSync(path.join(os.tmpdir(), "right-publish-test-"));
    const config = path.join(dir, "right-release.config.mjs");
    writeFileSync(config, `export default ${JSON.stringify({
      schema: 1, app: "fixture", version: "1.2.3", distribution: { provider: "r2-rightapps" },
      targets: { win: { installer: { artifacts: [{ file: "Fixture.msix", key: "fixture/installers/windows/current/Fixture.msix", versionedKey }] }, updater: { artifacts: [] } } },
    })};\n`);
    return spawnSync(process.execPath, [publisher, "--config", config, "--platform", "win", "--dry-run"], { encoding: "utf8", env: unmanagedEnv(process.env, { RIGHT_RELEASE_TIER: "patch" }) });
  };
  const ok = mk("fixture/installers/windows/versions/1.2.3.0/Fixture_1.2.3.0.msix");
  assert.equal(ok.status, 0, ok.stderr);
  assert.match(ok.stdout, /UPLOAD public\/fixture\/installers\/windows\/current\/Fixture\.msix/);
  assert.match(ok.stdout, /UPLOAD public\/fixture\/installers\/windows\/versions\/1\.2\.3\.0\/Fixture_1\.2\.3\.0\.msix/);
  const bad = mk("fixture/installers/windows/current/Fixture_1.2.3.0.msix");
  assert.notEqual(bad.status, 0);
  assert.match(bad.stderr, /versionedKey/);
});

// A real minisign key whose public text and signatures carry `keyIdHex`, standing in for keys the contract does not list.
function minisignKey(keyIdHex) {
  const { publicKey, privateKey } = generateKeyPairSync("ed25519");
  const keyIdLe = Buffer.from(keyIdHex, "hex").reverse();
  const publicKeyText = Buffer.from(`untrusted comment: minisign public key: ${keyIdHex}\n${Buffer.concat([Buffer.from("Ed"), keyIdLe, publicKey.export({ format: "der", type: "spki" }).subarray(-32)]).toString("base64")}\n`).toString("base64");
  const sign = (bytes) => {
    const raw = Buffer.concat([Buffer.from("Ed"), keyIdLe, cryptoSign(null, bytes, privateKey)]);
    const comment = "timestamp:1\tfile:fixture";
    const global = cryptoSign(null, Buffer.concat([raw.subarray(10), Buffer.from(comment)]), privateKey);
    return Buffer.from(`untrusted comment: signature from tauri secret key\n${raw.toString("base64")}\ntrusted comment: ${comment}\n${global.toString("base64")}\n`).toString("base64");
  };
  return { keyId: keyIdHex, publicKeyText, sign };
}

// Runs the production signature gate of publish-update.mjs (the declared-key read through the per-artifact checks) over stubs
// for the sealed release, which a script that reads the git checkout and the vault cannot otherwise be driven through.
// `keys` replaces the contract's key list so a suite app can be signed for; leave it out to use the real contract.
const publisherSource = readFileSync(publisher, "utf8").replace(/\r\n/g, "\n");
function sealedGate({ app, distribution = {}, updater = {}, bytes, signature, keys }) {
  const start = publisherSource.indexOf("const updaterPublicKey = ");
  const end = publisherSource.indexOf("let releaseToken = null;");
  assert.ok(start >= 0 && end > start, "the signature gate must sit before the release token is read");
  return runInNewContext(`${publisherSource.slice(start, end)}\ntrust;`, {
    path,
    config: { app, distribution },
    target: { updater },
    artifacts: [{ file: "Fixture.exe", signature: "Fixture.exe.sig" }],
    sealed: {},
    appInstallerKind: updater.kind === "appinstaller",
    signatureFor: () => signature,
    verifiedArtifact: () => ({}),
    assertArtifactUnchanged: () => bytes,
    resolveUpdaterTrust: (options) => resolveUpdaterTrust(keys ? { ...options, keys } : options),
    assertUpdaterKeyId: (text, id, label, options) => assertUpdaterKeyId(text, id, label, keys ? { ...options, keys } : options),
    assertUpdaterSignature,
  });
}

const gateBytes = Buffer.from("signed updater");
const listed = (...keys) => keys.map((key) => ({ kid: key.keyId, publicKey: key.publicKeyText, notAfter: null, notAfterUnix: null }));

test("a sealed publish for a suite app needs no key declaration and accepts the key its id names", () => {
  const current = minisignKey("1111111111111111");
  const trust = sealedGate({ app: "mailright", distribution: { updaterKeyId: current.keyId }, bytes: gateBytes, signature: current.sign(gateBytes), keys: listed(current) });
  assert.equal(trust.keyId, current.keyId);
  assert.equal(trust.publicKey, current.publicKeyText);
  // Naming the key is not enough: the bytes must still verify under it.
  assert.throws(
    () => sealedGate({ app: "mailright", distribution: { updaterKeyId: current.keyId }, bytes: Buffer.from("tampered"), signature: current.sign(gateBytes), keys: listed(current) }),
    /PUBLICATION_SIGNATURE_INVALID/,
  );
});

test("a sealed publish for a suite app refuses a signature by another key id, even a listed one", () => {
  const current = minisignKey("1111111111111111");
  const other = minisignKey("2222222222222222");
  assert.throws(
    () => sealedGate({ app: "mailright", distribution: { updaterKeyId: current.keyId }, bytes: gateBytes, signature: other.sign(gateBytes), keys: listed(current, other) }),
    (error) => error.code === "PUBLICATION_SIGNATURE_KEY_MISMATCH" && /signature of Fixture\.exe was not made by updater key 1111111111111111 \(found 2222222222222222\)/.test(error.message),
  );
});

test("a sealed publish for a suite app with no updaterKeyId is pinned to the contract signing key", () => {
  const stranger = minisignKey("3333333333333333");
  assert.throws(
    () => sealedGate({ app: "pulse", bytes: gateBytes, signature: stranger.sign(gateBytes) }),
    (error) => error.code === "PUBLICATION_SIGNATURE_KEY_MISMATCH" && error.message.includes(`was not made by updater key ${RIGHT_SUITE_UPDATER_SIGNING_KEY_ID} (found 3333333333333333)`),
  );
});

test("a sealed publish for a suite app refuses a declared key that is not the contract's, from any config field", () => {
  const contract = minisignKey("1111111111111111");
  const own = minisignKey("2222222222222222");
  const base = { app: "scraperight", bytes: gateBytes, signature: own.sign(gateBytes), keys: listed(contract) };
  const refused = (error) => error.code === "PUBLICATION_SIGNATURE_KEY_MISMATCH" && /suite apps do not declare their own/.test(error.message);
  for (const field of ["updaterPublicKey", "updaterPubkey"]) {
    assert.throws(() => sealedGate({ ...base, distribution: { [field]: own.publicKeyText, updaterKeyId: contract.keyId } }), refused);
  }
  for (const field of ["pubkey", "publicKey"]) {
    assert.throws(() => sealedGate({ ...base, distribution: { updaterKeyId: contract.keyId }, updater: { [field]: own.publicKeyText } }), refused);
  }
});

test("a sealed publish for a suite app refuses a key id the contract does not accept", () => {
  const own = minisignKey("1111111111111111");
  assert.throws(
    () => sealedGate({ app: "cutright", distribution: { updaterKeyId: own.keyId }, bytes: gateBytes, signature: own.sign(gateBytes) }),
    (error) => error.code === "PUBLICATION_SIGNATURE_KEY_MISMATCH" && /is not an updater key in the Right Suite contract/.test(error.message),
  );
});

test("a sealed publish for an app outside the suite still fails closed without a declared key, and a declared key needs no id", () => {
  const own = minisignKey("1111111111111111");
  const signature = own.sign(gateBytes);
  assert.throws(() => sealedGate({ app: "ember", bytes: gateBytes, signature }), (error) => error.code === "PUBLICATION_SIGNATURE_UNVERIFIABLE");
  const trust = sealedGate({ app: "ember", distribution: { updaterPublicKey: own.publicKeyText }, bytes: gateBytes, signature });
  assert.deepEqual({ ...trust }, { keyId: undefined, publicKey: own.publicKeyText, suite: false });
  assert.throws(() => sealedGate({ app: "ember", distribution: { updaterPublicKey: own.publicKeyText, updaterKeyId: "2222222222222222" }, bytes: gateBytes, signature }), /PUBLICATION_SIGNATURE_KEY_MISMATCH/);
  assert.throws(() => sealedGate({ app: "ember", distribution: { updaterPublicKey: own.publicKeyText }, bytes: Buffer.from("tampered"), signature }), /PUBLICATION_SIGNATURE_INVALID/);
});

test("a sealed publish of an MSIX update resolves no trust, as it has no minisign signature", () => {
  assert.equal(sealedGate({ app: "mailright", updater: { kind: "appinstaller" }, bytes: gateBytes, signature: "msix-authenticode-sha256:00" }), null);
});

test("the github lane pins each manifest signature to the id resolved from the contract, not to the raw config", () => {
  assert.match(publisherSource, /assertUpdaterKeyId\(signature, trust\.keyId, `signature of \$\{name\}`, \{ app: config\.app \}\)/);
  assert.doesNotMatch(publisherSource, /assertUpdaterKeyId\([^)]*config\.distribution\.updaterKeyId/);
  assert.match(publisherSource, /resolveUpdaterTrust\(\{ app: config\.app, configuredPublicKey: updaterPublicKey, configuredKeyId: config\.distribution\.updaterKeyId \}\)/);
});
