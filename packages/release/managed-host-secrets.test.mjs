import assert from "node:assert/strict";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";
import { spawnSync } from "./background-process.mjs";
import { publishR2Addon } from "./addon-command.mjs";
import { digestBytes } from "./publication-artifacts.mjs";
import { isManagedHost, MANAGED_HOST_ENV_SECRET_REFUSED, refuseManagedEnvSecret, refuseManagedEnvSecrets } from "./managed-host-secrets.mjs";
import { loadRuntimeArtifactSigningKey } from "./model-promote.mjs";
import { notarytoolAuthArgs } from "./notary-auth.mjs";
import { loadReleaseToken } from "./release-token.mjs";
import { signMacUpdaterFiles } from "./macos-signing-session.mjs";
import { runLargeUpload } from "./upload-large.mjs";

const helper = fileURLToPath(new URL("./sign-updater.mjs", import.meta.url));
const SOCKET_ENV = { RIGHTKIT_BUILD_BROKER_SOCKET: "/fake/rightkit-broker.sock" };
const SHIM_ENV = { PATH: "/usr/bin:/fake/.rightkit-managed/agent-bin:/bin" };
const REFUSED = { code: MANAGED_HOST_ENV_SECRET_REFUSED };

function tempDir(t) {
  const dir = mkdtempSync(path.join(os.tmpdir(), "rightkit-managed-secret-test-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  return dir;
}

// Replaces global fetch for the duration of fn, then restores it.
function withFetch(fn, impl) {
  const original = globalThis.fetch;
  globalThis.fetch = impl;
  return fn().finally(() => { globalThis.fetch = original; });
}

test("managed host is detected from the broker socket or a RightKit agent-bin on PATH", () => {
  assert.equal(isManagedHost(SOCKET_ENV), true);
  assert.equal(isManagedHost(SHIM_ENV), true);
  assert.equal(isManagedHost({ PATH: "/usr/bin:/bin" }), false);
  assert.equal(isManagedHost({}), false);
});

test("refusal is a typed error that names only the variable", () => {
  assert.throws(() => refuseManagedEnvSecret("FAKE_SECRET_NAME"), (error) => {
    assert.equal(error.code, MANAGED_HOST_ENV_SECRET_REFUSED);
    assert.equal(error.variable, "FAKE_SECRET_NAME");
    assert.match(error.message, /FAKE_SECRET_NAME/);
    assert.match(error.message, /Keychain|credential store/);
    return true;
  });
});

test("managed hosts refuse a set variable and ignore blank ones; unmanaged hosts allow", () => {
  assert.throws(() => refuseManagedEnvSecrets({ ...SOCKET_ENV, FAKE_VALUE_NAME: "fake-value" }, ["FAKE_VALUE_NAME"]), REFUSED);
  assert.throws(() => refuseManagedEnvSecrets({ ...SHIM_ENV, FAKE_VALUE_NAME: "fake-value" }, ["FAKE_VALUE_NAME"]), REFUSED);
  assert.doesNotThrow(() => refuseManagedEnvSecrets({ ...SOCKET_ENV, FAKE_VALUE_NAME: "   " }, ["FAKE_VALUE_NAME"]));
  assert.doesNotThrow(() => refuseManagedEnvSecrets({ FAKE_VALUE_NAME: "fake-value" }, ["FAKE_VALUE_NAME"]));
});

test("refusal messages never include the value", () => {
  assert.throws(() => refuseManagedEnvSecrets({ ...SOCKET_ENV, FAKE_VALUE_NAME: "fake-value-xyz" }, ["FAKE_VALUE_NAME"]), (error) => {
    assert.doesNotMatch(`${error.message}\n${error.stack}`, /fake-value-xyz/);
    return true;
  });
});

test("release token: managed hosts refuse environment token inputs; unmanaged hosts read them", async () => {
  await assert.rejects(loadReleaseToken({ env: { ...SOCKET_ENV, RIGHTAPPS_RELEASE_TOKEN: "fake-token-value" }, platform: "linux" }), REFUSED);
  await assert.rejects(loadReleaseToken({ env: { ...SOCKET_ENV, RIGHTAPPS_RELEASE_TOKEN_FILE: "/fake/release.token" }, platform: "linux" }), REFUSED);
  assert.equal(await loadReleaseToken({ env: { RIGHTAPPS_RELEASE_TOKEN: " fake-token-value " }, platform: "linux" }), "fake-token-value");
});

test("runtime artifact signing key: managed hosts refuse environment keys; unmanaged hosts read them", async () => {
  await assert.rejects(loadRuntimeArtifactSigningKey({ env: { ...SOCKET_ENV, RIGHTKIT_RUNTIME_ARTIFACT_PRIVATE_KEY: "fake-pem-value" }, platform: "darwin" }), REFUSED);
  await assert.rejects(loadRuntimeArtifactSigningKey({ env: { ...SOCKET_ENV, RIGHTKIT_RUNTIME_ARTIFACT_PRIVATE_KEY_FILE: "/fake/key.pem" }, platform: "darwin" }), REFUSED);
  assert.equal(await loadRuntimeArtifactSigningKey({ env: { RIGHTKIT_RUNTIME_ARTIFACT_PRIVATE_KEY: "fake-line1\\nline2" }, platform: "darwin" }), "fake-line1\nline2");
});

test("notary auth: managed hosts refuse environment API auth; keep the Keychain profile; unmanaged hosts use API auth", () => {
  assert.throws(() => notarytoolAuthArgs({ env: { ...SOCKET_ENV, APPLE_API_KEY_PATH: "/fake/AuthKey.p8" } }), REFUSED);
  assert.throws(() => notarytoolAuthArgs({ env: { ...SOCKET_ENV, APPLE_API_KEY: "FAKEKEYID" } }), REFUSED);
  assert.deepEqual(notarytoolAuthArgs({ env: SOCKET_ENV }), ["--keychain-profile", "apple-dev-notary"]);
  assert.deepEqual(notarytoolAuthArgs({ profile: "apple-dev-notary", env: { ...SOCKET_ENV, APPLE_API_KEY_PATH: "/fake/AuthKey.p8" } }), ["--keychain-profile", "apple-dev-notary"]);
  assert.deepEqual(
    notarytoolAuthArgs({ env: { APPLE_API_KEY_PATH: "/fake/AuthKey.p8", APPLE_API_KEY: "FAKEKEYID", APPLE_API_ISSUER: "FAKE-ISSUER" } }),
    ["--key", "/fake/AuthKey.p8", "--key-id", "FAKEKEYID", "--issuer", "FAKE-ISSUER"],
  );
});

test("macOS updater signing: managed hosts refuse inherited key or password before any signing command", () => {
  for (const secret of [{ TAURI_SIGNING_PRIVATE_KEY: "fake-key-value" }, { TAURI_SIGNING_PRIVATE_KEY_PASSWORD: "fake-password-value" }]) {
    const calls = [];
    const run = (command, args, options) => { calls.push({ command, args, options }); return { status: 0, stdout: "", stderr: "" }; };
    assert.throws(() => signMacUpdaterFiles(["/fake/updater.tar.gz"], { env: { ...SOCKET_ENV, ...secret }, run }), REFUSED);
    assert.equal(calls.length, 0);
  }
});

test("macOS updater signing: unmanaged hosts keep the inherited key path", () => {
  const calls = [];
  const run = (command, args, options) => { calls.push({ command, args, options }); return { status: 0, stdout: "", stderr: "" }; };
  assert.equal(signMacUpdaterFiles(["/fake/updater.tar.gz"], { env: { TAURI_SIGNING_PRIVATE_KEY: "fake-key-value" }, run }).length, 1);
  assert.equal(calls.length, 1);
  assert.equal(calls[0].options.env.TAURI_SIGNING_PRIVATE_KEY, "fake-key-value");
});

test("updater signer script: managed hosts refuse inherited key or password before signing", () => {
  const key = spawnSync(process.execPath, [helper, "--file", "Fixture.exe"], {
    encoding: "utf8",
    env: { ...SOCKET_ENV, TAURI_SIGNING_PRIVATE_KEY: "fake-key-value" },
  });
  assert.equal(key.status, 1);
  assert.match(key.stderr, /MANAGED_HOST_ENV_SECRET_REFUSED.*TAURI_SIGNING_PRIVATE_KEY/);
  assert.doesNotMatch(`${key.stdout}${key.stderr}`, /fake-key-value/);

  const password = spawnSync(process.execPath, [helper, "--file", "Fixture.exe"], {
    encoding: "utf8",
    env: { ...SOCKET_ENV, TAURI_SIGNING_PRIVATE_KEY_PASSWORD: "fake-password-value" },
  });
  assert.equal(password.status, 1);
  assert.match(password.stderr, /MANAGED_HOST_ENV_SECRET_REFUSED.*TAURI_SIGNING_PRIVATE_KEY_PASSWORD/);
  assert.doesNotMatch(`${password.stdout}${password.stderr}`, /fake-password-value/);
});

test("updater signer script: unmanaged hosts accept an inherited key and stop at the artifact check", () => {
  // The artifact is absent, so the run stops before any signer executes.
  const result = spawnSync(process.execPath, [helper, "--file", "absent-fixture.exe"], {
    encoding: "utf8",
    env: { TAURI_SIGNING_PRIVATE_KEY: "fake-key-value" },
  });
  assert.equal(result.status, 1);
  assert.match(result.stderr, /updater artifact not found/);
  assert.doesNotMatch(result.stderr, /MANAGED_HOST_ENV_SECRET_REFUSED/);
});

test("R2 add-on publish: managed hosts keep the environment Cloudflare token path (no protected loader yet)", () => {
  // The manifest is absent, so the run stops at the read before any mutation; the token is never refused.
  const input = { manifestPath: "/fake/absent-addon-manifest.json", repoRoot: "/fake", config: {}, commit: "a".repeat(40), platform: "mac" };
  assert.throws(
    () => publishR2Addon(input, { run: () => { throw new Error("no upload expected"); }, env: { ...SOCKET_ENV, CLOUDFLARE_API_TOKEN: "fake-token-value" } }),
    (error) => error.code !== MANAGED_HOST_ENV_SECRET_REFUSED,
  );
});

test("large upload: managed hosts keep forwarding the environment Cloudflare token to the uploader (no protected loader yet)", (t) => {
  const dir = tempDir(t);
  const file = path.join(dir, "Artifact.exe");
  writeFileSync(file, "artifact");
  const evidence = path.join(dir, "evidence.json");
  writeFileSync(evidence, JSON.stringify({ artifactSha256: digestBytes(Buffer.from("artifact")) }));
  const calls = [];
  const run = (command, args) => { calls.push([command, args]); return { status: 0 }; };
  assert.doesNotThrow(() => runLargeUpload([file, "fixture/updates/windows/current/Artifact.exe", "public", "--evidence", evidence], {
    run,
    env: { ...SOCKET_ENV, CLOUDFLARE_API_TOKEN: "fake-token-value", RIGHT_RELEASE_UPLOAD_DRY_RUN: "1" },
  }));
  assert.equal(calls[0][0], "pnpm");
});
