import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { publishR2Addon } from "./addon-command.mjs";
import { digestBytes } from "./publication-artifacts.mjs";
import { MANAGED_HOST_ENV_SECRET_REFUSED } from "./managed-host-secrets.mjs";
import {
  createCredentialStore,
  execWithUpdaterCredential,
  PROTECTED_SECRET_MISSING,
  ProtectedSecretMissingError,
  protectedSecretStoreCommand,
  resolveCloudflareApiToken,
  resolveEnvBackedSecret,
} from "./protected-credentials.mjs";
import { pruneReleaseObjects } from "./prune-r2.mjs";
import { runLargeUpload } from "./upload-large.mjs";
import { spawnSync } from "./background-process.mjs";

const SOCKET_ENV = { RIGHTKIT_BUILD_BROKER_SOCKET: "/fake/rightkit-broker.sock" };
const REFUSED = { code: MANAGED_HOST_ENV_SECRET_REFUSED };
const MISSING = { code: PROTECTED_SECRET_MISSING };

// Fake Keychain: items keyed by service name; records every read.
function fakeKeychain(items = {}) {
  const reads = [];
  const readKeychain = (query) => { reads.push(query); return items[query.service]; };
  return { readKeychain, reads };
}
// Fake Windows token files: items keyed by file basename; records every read.
function fakeFiles(items = {}) {
  const reads = [];
  const readFile = (file) => { reads.push(file); return items[path.win32.basename(file)]; };
  return { readFile, reads };
}
const darwinStore = (items, env = { USER: "fake-user" }) => createCredentialStore({ env, platform: "darwin", ...fakeKeychain(items) });
const emptyDarwinStore = () => darwinStore({});

function tempDir(t) {
  const dir = mkdtempSync(path.join(os.tmpdir(), "rightkit-protected-credentials-test-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  return dir;
}

async function withFetch(impl, fn) {
  const original = globalThis.fetch;
  globalThis.fetch = impl;
  try { return await fn(); } finally { globalThis.fetch = original; }
}

test("store commands prompt for the value and never place it on the command line", () => {
  assert.equal(protectedSecretStoreCommand("cloudflare-api-token", "darwin"), 'security add-generic-password -a "$USER" -s rightkit-cloudflare-api-token -U -w');
  const windows = protectedSecretStoreCommand("cloudflare-api-token", "win32");
  assert.match(windows, /Read-Host -AsSecureString/);
  assert.match(windows, /RightKit'/);
  assert.match(windows, /cloudflare-api-token\.token/);
  assert.throws(() => protectedSecretStoreCommand("apple-asc-key-id", "darwin"), /unknown protected secret/, "no Apple store entries remain");
  assert.equal(protectedSecretStoreCommand("cloudflare-api-token", "linux"), null);
});

test("macOS reads the Keychain generic password for the current account and trims it", () => {
  const { readKeychain, reads } = fakeKeychain({ "rightkit-cloudflare-api-token": "  fake-keychain-value\n" });
  const store = createCredentialStore({ env: { USER: "fake-user" }, platform: "darwin", readKeychain });
  assert.equal(store.read("cloudflare-api-token"), "fake-keychain-value");
  assert.deepEqual(reads, [{ service: "rightkit-cloudflare-api-token", account: "fake-user" }]);
});

test("Windows reads the protected token file under APPDATA\\RightKit and trims it", () => {
  const { readFile, reads } = fakeFiles({ "cloudflare-api-token.token": "fake-file-value\r\n" });
  const store = createCredentialStore({ env: { APPDATA: "C:\\fake\\AppData" }, platform: "win32", readFile });
  assert.equal(store.read("cloudflare-api-token"), "fake-file-value");
  assert.deepEqual(reads, [path.win32.join("C:\\fake\\AppData", "RightKit", "cloudflare-api-token.token")]);
});

test("updater credentials have no file/export/fallback path on any host", () => {
  let reads = 0;
  for (const platform of ["win32", "darwin", "linux"]) {
    const store = createCredentialStore({ platform, env: {}, readFile: () => { reads++; }, readKeychain: () => { reads++; } });
    assert.throws(() => store.read("updater-signing-key"), MISSING);
    assert.throws(() => resolveEnvBackedSecret({ name: "updater-signing-key", envName: "TAURI_SIGNING_PRIVATE_KEY", env: { TAURI_SIGNING_PRIVATE_KEY: "fixture-key" }, credentialStore: store }), /execution boundary/);
  }
  assert.equal(reads, 0);
  const command = protectedSecretStoreCommand("updater-signing-key", "win32");
  assert.match(command, /\| & rightkit-credentials\.exe set --namespace 'rightsuite-updater-key'/);
  assert.match(command, /^\$rightkitSecure = Read-Host -AsSecureString/);
  assert.doesNotMatch(command, /powershell|-Command/);
  assert.doesNotMatch(command, /WriteAllText|APPDATA|\.token/);
  assert.match(protectedSecretStoreCommand("updater-signing-key", "darwin"), /rightsuite-updater-key/);
});

test("updater provider trace uses M11 child execution & preserves exit status", () => {
  let call;
  const status = { status: 17 };
  const result = execWithUpdaterCredential(process.execPath, ["fixture-pnpm.cjs", "exec", "tauri", "signer", "sign", "fixture & literal.exe"], {
    account: "fixture-user", provider: "fixture-credentials.exe", env: {},
    run: (...args) => { call = args; return status; },
  });
  assert.equal(result, status);
  assert.deepEqual(call[1], ["exec", "--namespace", "rightsuite-updater-key", "--account", "fixture-user", "--env", "TAURI_SIGNING_PRIVATE_KEY", "--", process.execPath, "fixture-pnpm.cjs", "exec", "tauri", "signer", "sign", "fixture & literal.exe"]);
  assert.equal(call[2].env.TAURI_SIGNING_PRIVATE_KEY, undefined);
  assert.equal(call[2].shell, false);
  assert.equal(call[2].windowsHide, true);
});

test("Windows updater provisioning hint parses directly in PowerShell without execution", { skip: process.platform !== "win32" }, () => {
  const command = protectedSecretStoreCommand("updater-signing-key", "win32");
  const result = spawnSync("powershell.exe", ["-NoProfile", "-NonInteractive", "-Command", "$rightkitTokens = $null; $rightkitErrors = $null; [void][System.Management.Automation.Language.Parser]::ParseInput([Console]::In.ReadToEnd(), [ref]$rightkitTokens, [ref]$rightkitErrors); exit $rightkitErrors.Count"], { input: command, encoding: "utf8", shell: false });
  assert.equal(result.status, 0, result.stderr);
});

test("updater provider rejects inherited secrets & relative child before launching", () => {
  let calls = 0;
  const run = () => { calls++; };
  assert.throws(() => execWithUpdaterCredential("node", [], { env: {}, run }), /absolute native/);
  assert.throws(() => execWithUpdaterCredential(process.execPath, [], { env: { tauri_signing_private_key: "fixture" }, run }), /OS credential provider/);
  assert.equal(calls, 0);
});

test("missing updater provider explains required installation without credential lookup", () => {
  assert.throws(() => execWithUpdaterCredential(process.execPath, [], { env: {}, run: () => ({ error: { code: "ENOENT" } }) }), /install rightkit-credentials >=0\.2\.2 on PATH/);
});

test("absent or blank store entries throw a typed error carrying the one-line store command", () => {
  for (const items of [{}, { "rightkit-cloudflare-api-token": "  \n" }]) {
    const store = darwinStore(items);
    assert.throws(() => store.read("cloudflare-api-token"), (error) => {
      assert.ok(error instanceof ProtectedSecretMissingError);
      assert.equal(error.code, PROTECTED_SECRET_MISSING);
      assert.equal(error.secret, "cloudflare-api-token");
      assert.equal(error.storeCommand, protectedSecretStoreCommand("cloudflare-api-token", "darwin"));
      assert.match(error.message, /security add-generic-password -a "\$USER" -s rightkit-cloudflare-api-token -U -w/);
      assert.doesNotMatch(error.message, /fake-/);
      return true;
    });
  }
});

test("platforms without a protected store report absence without a store command", () => {
  const store = createCredentialStore({ env: {}, platform: "linux" });
  assert.throws(() => store.read("cloudflare-api-token"), (error) => error.code === PROTECTED_SECRET_MISSING && error.storeCommand === null);
});

test("managed hosts refuse the environment variable even when the protected store holds a value", () => {
  const { readKeychain, reads } = fakeKeychain({ "rightkit-cloudflare-api-token": "fake-store-token" });
  const store = createCredentialStore({ env: { USER: "fake-user" }, platform: "darwin", readKeychain });
  assert.throws(() => resolveEnvBackedSecret({ name: "cloudflare-api-token", envName: "CLOUDFLARE_API_TOKEN", env: { ...SOCKET_ENV, CLOUDFLARE_API_TOKEN: "fake-env-token" }, credentialStore: store }), REFUSED);
  assert.equal(reads.length, 0, "refusal happens before any credential read");
});

test("managed hosts never fall back to the environment when the protected store is empty", () => {
  // The variable is blank here, so refusal does not apply and the missing store entry must still fail.
  assert.throws(() => resolveCloudflareApiToken({ env: { ...SOCKET_ENV, CLOUDFLARE_API_TOKEN: "  " }, credentialStore: emptyDarwinStore() }), MISSING);
});

test("unmanaged hosts prefer the protected store, then the environment, then fail with the store command", () => {
  assert.equal(resolveCloudflareApiToken({ env: { CLOUDFLARE_API_TOKEN: "fake-env-token" }, credentialStore: darwinStore({ "rightkit-cloudflare-api-token": "fake-store-token" }) }), "fake-store-token");
  assert.equal(resolveCloudflareApiToken({ env: { CLOUDFLARE_API_TOKEN: " fake-env-token " }, credentialStore: emptyDarwinStore() }), "fake-env-token");
  assert.throws(() => resolveCloudflareApiToken({ env: {}, credentialStore: emptyDarwinStore() }), (error) => error.code === PROTECTED_SECRET_MISSING && /store it once with: security add-generic-password/.test(error.message));
});

test("unmanaged resolution skipped entirely for dry runs; refusal still applies on managed hosts", () => {
  const { readKeychain, reads } = fakeKeychain({});
  const store = createCredentialStore({ env: {}, platform: "darwin", readKeychain });
  assert.equal(resolveCloudflareApiToken({ env: {}, credentialStore: store, required: false }), undefined);
  assert.equal(reads.length, 0);
  assert.throws(() => resolveCloudflareApiToken({ env: { ...SOCKET_ENV, CLOUDFLARE_API_TOKEN: "fake-env-token" }, credentialStore: store, required: false }), REFUSED);
});

test("non-missing store failures propagate instead of falling back to the environment", () => {
  const store = createCredentialStore({ env: {}, platform: "darwin", readKeychain: () => { throw new Error("fake keychain failure"); } });
  assert.throws(() => resolveCloudflareApiToken({ env: { CLOUDFLARE_API_TOKEN: "fake-env-token" }, credentialStore: store }), /fake keychain failure/);
});

test("R2 add-on publish: the protected token is resolved before the manifest read, and absence fails with the store command", () => {
  const calls = [];
  assert.throws(
    () => publishR2Addon({ manifestPath: "/fake/absent-addon-manifest.json", repoRoot: "/fake", config: {}, commit: "a".repeat(40), platform: "mac" }, {
      run: (...args) => { calls.push(args); return { status: 0 }; },
      env: {},
      credentialStore: emptyDarwinStore(),
    }),
    MISSING,
  );
  assert.equal(calls.length, 0);
});

test("prune: managed hosts refuse the environment token before any Cloudflare request, dry run included", async () => {
  let fetched = 0;
  await withFetch(async () => { fetched += 1; throw new Error("no request expected"); }, async () => {
    for (const dryRun of [false, true]) {
      await assert.rejects(pruneReleaseObjects({ app: "fake-app", platform: "mac", keep: {}, dryRun, env: { ...SOCKET_ENV, CLOUDFLARE_API_TOKEN: "fake-env-token" }, credentialStore: emptyDarwinStore() }), REFUSED);
    }
  });
  assert.equal(fetched, 0);
});

test("prune: unmanaged hosts authenticate with the protected token, then the environment, and fail closed without either", async () => {
  const authorizations = [];
  const fetchStub = async (url, options) => {
    authorizations.push(options.headers.authorization);
    return { ok: true, status: 200, json: async () => ({ success: true, result: [] }) };
  };
  await withFetch(fetchStub, async () => {
    await pruneReleaseObjects({ app: "fake-app", platform: "mac", keep: {}, env: {}, credentialStore: darwinStore({ "rightkit-cloudflare-api-token": "fake-store-token" }) });
    await pruneReleaseObjects({ app: "fake-app", platform: "mac", keep: {}, env: { CLOUDFLARE_API_TOKEN: "fake-env-token" }, credentialStore: emptyDarwinStore() });
    await assert.rejects(pruneReleaseObjects({ app: "fake-app", platform: "mac", keep: {}, env: {}, credentialStore: emptyDarwinStore() }), MISSING);
  });
  assert.ok(authorizations.length > 0);
  assert.ok(authorizations.slice(0, 2).every((value) => value === "Bearer fake-store-token"));
  assert.ok(authorizations.slice(2).every((value) => value === "Bearer fake-env-token"));
  assert.equal(authorizations.length, 4, "one list request per bucket for each successful prune; the failing prune makes no request");
});

function uploadFixture(t) {
  const dir = tempDir(t);
  const file = path.join(dir, "Artifact.exe");
  writeFileSync(file, "artifact");
  const evidence = path.join(dir, "evidence.json");
  writeFileSync(evidence, JSON.stringify({ artifactSha256: digestBytes(Buffer.from("artifact")) }));
  return { file, evidence };
}

// Fake Wrangler: put stores the uploaded bytes, get returns them; every call is recorded with its env.
function fakeWrangler() {
  const calls = [];
  let remote;
  const run = (command, args, options) => {
    calls.push({ command, args, env: options?.env });
    if (args[0] === "--version") return { status: 0 };
    if (args.includes("put")) { remote = readFileSync(args[args.indexOf("--file") + 1]); return { status: 0 }; }
    if (args.includes("get")) { writeFileSync(args[args.indexOf("--file") + 1], remote); return { status: 0 }; }
    throw new Error(`unexpected call ${args}`);
  };
  return { calls, run };
}

test("large upload: the protected token reaches only the Wrangler child environment", (t) => {
  const { file, evidence } = uploadFixture(t);
  const wrangler = fakeWrangler();
  const callerEnv = { CLOUDFLARE_ACCOUNT_ID: "fake-account" };
  runLargeUpload([file, "fixture/file", "private", "--evidence", evidence], { run: wrangler.run, env: callerEnv, credentialStore: darwinStore({ "rightkit-cloudflare-api-token": "fake-store-token" }) });
  const put = wrangler.calls.find((call) => call.args.includes("put"));
  assert.equal(put.env.CLOUDFLARE_API_TOKEN, "fake-store-token");
  assert.equal(callerEnv.CLOUDFLARE_API_TOKEN, undefined, "the caller's environment object is never mutated");
});

test("large upload: unmanaged hosts fall back to the environment token when the store is empty", (t) => {
  const { file, evidence } = uploadFixture(t);
  const wrangler = fakeWrangler();
  runLargeUpload([file, "fixture/file", "public", "--evidence", evidence], { run: wrangler.run, env: { CLOUDFLARE_API_TOKEN: "fake-env-token" }, credentialStore: emptyDarwinStore() });
  assert.equal(wrangler.calls.find((call) => call.args.includes("put")).env.CLOUDFLARE_API_TOKEN, "fake-env-token");
});

test("large upload: a real upload with no token fails with the store command before any Wrangler call", (t) => {
  const { file, evidence } = uploadFixture(t);
  const wrangler = fakeWrangler();
  assert.throws(() => runLargeUpload([file, "fixture/file", "public", "--evidence", evidence], { run: wrangler.run, env: {}, credentialStore: emptyDarwinStore() }), MISSING);
  assert.equal(wrangler.calls.length, 0);
});

test("large upload: dry runs need no token on unmanaged hosts; managed hosts still refuse the environment token", (t) => {
  const { file, evidence } = uploadFixture(t);
  const wrangler = fakeWrangler();
  const dry = { RIGHT_RELEASE_UPLOAD_DRY_RUN: "1" };
  assert.equal(runLargeUpload([file, "fixture/file", "public", "--evidence", evidence], { run: wrangler.run, env: dry, credentialStore: emptyDarwinStore() }).sha256, digestBytes(Buffer.from("artifact")));
  assert.throws(() => runLargeUpload([file, "fixture/file", "public", "--evidence", evidence], { run: wrangler.run, env: { ...dry, ...SOCKET_ENV, CLOUDFLARE_API_TOKEN: "fake-env-token" }, credentialStore: emptyDarwinStore() }), REFUSED);
  assert.equal(wrangler.calls.filter((call) => call.args.includes("put")).length, 0);
});
