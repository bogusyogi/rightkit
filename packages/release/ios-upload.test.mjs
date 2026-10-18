import assert from "node:assert/strict";
import { createHash, generateKeyPairSync } from "node:crypto";
import fs, { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { syncBuiltinESMExports } from "node:module";
import path from "node:path";
import test, { mock } from "node:test";
import { fakeAsc } from "./test-support/fake-asc.mjs";
import { tempRoot } from "./test-support/temp-roots.mjs";
import {
  IOS_DISTRIBUTED_MARKER, IOS_EVIDENCE_NAME, IOS_UPLOADED_MARKER, IosUploadError, authorizeIosUpload, buildUploadJournal, iosUploadPlan, main, runIosUpload, validateIosDistribution,
} from "./ios-upload.mjs";
import { readVerifiedArtifact } from "./publication-artifacts.mjs";

const APP_ID = "6806854037";
const BUNDLE = "app.heardright.ios.native";
const KEY_ID = "ABC123DEFG";
const ISSUER = "69a6de80-0b1c-47e3-e053-5b8c7c11a4d1";
const RELEASE = "HeardRight-1.0.214-abcd1234";
const { privateKey } = generateKeyPairSync("ec", { namedCurve: "prime256v1" });
const distribution = (over = {}) => ({ provider: "app-store-connect", appId: APP_ID, bundleId: BUNDLE, groups: "all", submitExternalBetaReview: true, ...over });
const refused = (code, fn) => assert.throws(fn, (error) => error instanceof IosUploadError && error.code === code, code);

test("a distribution block is validated strictly: groups is required and external beta review is off unless asked for", () => {
  const parsed = validateIosDistribution(distribution());
  assert.equal(parsed.track, "testflight");
  assert.equal(parsed.groups, "all");
  assert.equal(parsed.submitExternalBetaReview, true);
  const bare = { provider: "app-store-connect", appId: APP_ID, bundleId: BUNDLE };
  refused("IOS_UPLOAD_CONFIG_INVALID", () => validateIosDistribution(bare));
  assert.equal(validateIosDistribution({ ...bare, groups: ["Internal Dogfood"] }).submitExternalBetaReview, false);
  assert.equal(parsed.waitTimeoutSeconds, 1800);
  assert.equal(parsed.credentialServices.keyService, "rightkit-asc-api-key");
  assert.deepEqual(validateIosDistribution(distribution({ groups: ["Internal Dogfood"], credentials: { keyService: "x.key" } })).groups, ["Internal Dogfood"]);
  refused("IOS_UPLOAD_CONFIG_INVALID", () => validateIosDistribution(undefined));
  refused("IOS_UPLOAD_CONFIG_INVALID", () => validateIosDistribution(distribution({ provider: "google-play" })));
  refused("IOS_UPLOAD_CONFIG_INVALID", () => validateIosDistribution(distribution({ appId: "app" })));
  refused("IOS_UPLOAD_CONFIG_INVALID", () => validateIosDistribution(distribution({ colour: "red" })));
  refused("IOS_UPLOAD_CONFIG_INVALID", () => validateIosDistribution(distribution({ waitTimeoutSeconds: 5 })));
  refused("IOS_UPLOAD_CONFIG_INVALID", () => validateIosDistribution(distribution({ groups: [] })));
});

test("there is no App Store submission and no test bundle id", () => {
  refused("IOS_UPLOAD_TRACK_REFUSED", () => validateIosDistribution(distribution({ track: "production" })));
  refused("IOS_UPLOAD_TEST_BUNDLE", () => validateIosDistribution(distribution({ bundleId: `${BUNDLE}.dev` })));
});

test("a key path, key id, issuer, inline key, env name or password in the distribution block is refused", () => {
  for (const key of ["keyPath", "keyFile", "keyId", "issuer", "issuerId", "p8", "privateKey", "apiKey", "envKey", "token", "password", "secretFile"]) {
    refused("IOS_ASC_CREDENTIAL_SOURCE_REFUSED", () => validateIosDistribution(distribution({ [key]: "x" })));
  }
  refused("IOS_ASC_CREDENTIAL_SOURCE_REFUSED", () => validateIosDistribution(distribution({ credentials: { keyPath: "~/AuthKey.p8" } })));
  refused("IOS_ASC_CREDENTIAL_SOURCE_REFUSED", () => validateIosDistribution(distribution({ credentials: { keyId: KEY_ID } })));
});

// ---- the run ------------------------------------------------------------------------------------------------------
function fixture(over = {}) {
  const repoRoot = tempRoot("rk-ios-upload-");
  const sealedDir = path.join(repoRoot, ".right-release", "sealed", RELEASE, "ios");
  mkdirSync(sealedDir, { recursive: true });
  const bytes = Buffer.from("IPA-BYTES");
  const file = path.join(sealedDir, "HeardRight.ipa");
  writeFileSync(file, bytes);
  const sha256 = createHash("sha256").update(bytes).digest("hex");
  const ipa = readVerifiedArtifact(file, { sha256, sizeBytes: bytes.length });
  const auth = {
    repoRoot, sealedDir, configPath: path.join(repoRoot, "right-release.config.mjs"), ipa,
    entry: { role: "ipa", name: "HeardRight.ipa", sha256, sizeBytes: bytes.length },
    distribution: validateIosDistribution(distribution(over.distribution)),
    receipt: { marketingVersion: "1.0.214", buildNumber: "2026101801", bundleId: BUNDLE, teamId: "6KLGD3LLKF" },
  };
  const ASC_ORIGIN = "https://api.appstoreconnect.apple.com";
  const logs = [];
  const asc = over.asc ?? fakeAsc({ ...over.fake, groups: [{ id: "g1", name: "Internal Dogfood", internal: true }] });
  const fetchCalls = [];
  let clock = 0;
  const deps = {
    releaseId: RELEASE, env: { USER: "tester", PATH: "/usr/bin", APPLE_API_KEY_PATH: "" }, platform: "darwin",
    authorize: async () => auth,
    readCredentials: over.readCredentials ?? (() => ({ keyId: KEY_ID, issuer: ISSUER, privateKey })),
    fetchImpl: async (...args) => { fetchCalls.push(args); return asc.fetchImpl(...args); },
    detectSession: () => ({ guiRunner: false, reason: "test Aqua session" }),
    sleep: async (ms) => { clock += ms; }, monotonic: () => clock, now: () => new Date("2026-10-18T10:00:00Z"), log: (line) => logs.push(line),
  };
  return { auth, deps, logs, fetchCalls, asc, repoRoot, stateRoot: path.join(repoRoot, ".right-release", "state", RELEASE, "ios") };
}

test("dry run is the default: the plan is printed and nothing is read, run or sent", async () => {
  const f = fixture({ readCredentials: () => assert.fail("a dry run reads no Keychain item") });
  const result = await runIosUpload({ ...f.deps, fetchImpl: () => assert.fail("a dry run sends nothing") });
  assert.equal(result.applied, false);
  assert.equal(result.plan.appId, APP_ID);
  assert.deepEqual(result.plan.steps.map((step) => step.id), ["buildUploads.find", "buildUploads.create", "buildUploadFiles.reserve", "uploadOperations.put", "buildUploadFiles.commit", "buildUploads.poll", "builds.wait", "betaGroups.list", "betaGroups.assign", "betaAppReviewSubmissions.create"]);
  assert.match(f.logs.join("\n"), /dry-run \(default; pass --apply/);
  assert.ok(!f.logs.join("\n").match(/PRIVATE KEY|Bearer/));
  assert.ok(!existsSync(f.stateRoot));
  assert.equal(result.plan.credentials.source, "macOS login Keychain");
});

test("apply is refused off macOS, and --apply with --dry-run is a usage error", async () => {
  const f = fixture();
  await assert.rejects(runIosUpload({ ...f.deps, apply: true, platform: "win32" }), (error) => error instanceof IosUploadError && error.code === "IOS_HOST_UNSUPPORTED");
  assert.equal(await main({ releaseId: RELEASE, apply: true, dryRun: true }), 2);
});

test("ASC key material in the environment is refused on any host, and ASC variables on a managed host, before anything else", async () => {
  const f = fixture();
  const authorize = () => assert.fail("refused before the seal is even read");
  await assert.rejects(runIosUpload({ ...f.deps, authorize, env: { ...f.deps.env, SOME_VAR: "-----BEGIN PRIVATE KEY-----\nabc" } }), (error) => error.code === "IOS_ASC_ENV_SECRET_REFUSED");
  await assert.rejects(runIosUpload({ ...f.deps, authorize, env: { ...f.deps.env, RIGHTKIT_BUILD_BROKER_SOCKET: "/tmp/s", APPLE_API_ISSUER: ISSUER } }), (error) => error.code === "MANAGED_HOST_ENV_SECRET_REFUSED");
});

test("apply uploads in process through the build-upload resources, then distributes the exact build", async () => {
  const f = fixture();
  const result = await runIosUpload({ ...f.deps, apply: true });
  assert.equal(result.applied, true);
  const api = f.asc.requests.filter((request) => request.path.startsWith("/v1/"));
  const calls = api.map((request) => `${request.method} ${request.path}`);
  assert.deepEqual(calls.slice(0, 5), [`GET /v1/apps/${APP_ID}/buildUploads`, "POST /v1/buildUploads", "POST /v1/buildUploadFiles", "PATCH /v1/buildUploadFiles/bf1", "GET /v1/buildUploads/bu1"]);
  const create = api[1].body.data;
  assert.deepEqual(create.attributes, { cfBundleShortVersionString: "1.0.214", cfBundleVersion: "2026101801", platform: "IOS" });
  assert.deepEqual(create.relationships.app.data, { type: "apps", id: APP_ID });
  const reserve = api[2].body.data;
  assert.deepEqual(reserve.attributes, { assetType: "ASSET", fileName: "HeardRight.ipa", fileSize: 9, uti: "com.apple.ipa" });
  assert.deepEqual(reserve.relationships.buildUpload.data, { type: "buildUploads", id: "bu1" });
  // the bytes arrive in the reserved parts, in order, with the reserved headers and without the bearer token
  assert.equal(f.asc.puts.length, 3, "the fake splits nine bytes into three parts");
  assert.equal(Buffer.concat(f.asc.puts.map((put) => put.body)).toString(), "IPA-BYTES");
  assert.ok(f.asc.puts.every((put) => put.method === "PUT" && put.headers["Content-Type"] === "application/octet-stream" && !("Authorization" in put.headers)));
  const commit = f.asc.requests.find((request) => request.method === "PATCH").body.data;
  assert.equal(commit.attributes.uploaded, true);
  assert.deepEqual(commit.attributes.sourceFileChecksums.file, { hash: f.auth.entry.sha256, algorithm: "SHA_256" });
  assert.equal(result.evidence.testflight.buildId, "b1");
  const evidence = JSON.parse(readFileSync(path.join(f.repoRoot, ".right-release", "receipts", IOS_EVIDENCE_NAME), "utf8"));
  assert.equal(evidence.ipa.sha256, f.auth.entry.sha256);
  assert.equal(evidence.testflight.groups[0].assigned, true);
  assert.ok(existsSync(path.join(f.stateRoot, IOS_UPLOADED_MARKER)));
  assert.ok(existsSync(path.join(f.stateRoot, IOS_DISTRIBUTED_MARKER)));
  const everything = JSON.stringify(evidence) + readFileSync(path.join(f.stateRoot, IOS_DISTRIBUTED_MARKER), "utf8") + readFileSync(path.join(f.stateRoot, IOS_UPLOADED_MARKER), "utf8") + f.logs.join("\n");
  assert.ok(!everything.match(/PRIVATE KEY|Bearer|ya29|secret-signature/));
});

test("D4: no file containing key material is ever written during an apply", async () => {
  const f = fixture();
  const derBase64 = privateKey.export({ type: "pkcs8", format: "der" }).toString("base64");
  const writes = [];
  const record = (name) => (original) => function recorded(target, data, ...rest) {
    writes.push({ name, target: String(target), data: Buffer.isBuffer(data) || ArrayBuffer.isView(data) ? Buffer.from(data).toString("latin1") : String(data ?? "") });
    return original.call(this, target, data, ...rest);
  };
  const spies = [
    mock.method(fs, "writeFileSync", record("writeFileSync")(fs.writeFileSync)),
    mock.method(fs, "appendFileSync", record("appendFileSync")(fs.appendFileSync)),
  ];
  const mkdtemps = mock.method(fs, "mkdtempSync");
  syncBuiltinESMExports();
  try { await runIosUpload({ ...f.deps, apply: true }); }
  finally { for (const spy of [...spies, mkdtemps]) spy.mock.restore(); syncBuiltinESMExports(); }
  assert.ok(writes.length > 0, "the spy saw the marker and evidence writes");
  assert.equal(mkdtemps.mock.callCount(), 0, "no temp directory is created for a key");
  for (const write of writes) {
    assert.ok(!write.data.includes("PRIVATE KEY") && !write.data.includes(derBase64), `${write.name} ${path.basename(write.target)} holds key material`);
    assert.ok(!/AuthKey_|\.p8$/i.test(write.target), `${write.target} looks like a key file`);
  }
});

test("a second apply after full success does nothing", async () => {
  const f = fixture();
  await runIosUpload({ ...f.deps, apply: true });
  const again = await runIosUpload({ ...f.deps, apply: true, run: () => assert.fail("no second upload"), readCredentials: () => assert.fail("no credential read") });
  assert.equal(again.alreadyDistributed, true);
});

test("a refused upload part stops the upload with no marker, and the pre-signed URL is not echoed", async () => {
  const f = fixture({ fake: { failPut: true } });
  await assert.rejects(runIosUpload({ ...f.deps, apply: true }), (error) => error.code === "IOS_BUILD_UPLOAD_FAILED" && /HTTP 403/.test(error.message) && !error.message.includes("secret-signature"));
  assert.ok(!existsSync(path.join(f.stateRoot, IOS_UPLOADED_MARKER)));
  assert.ok(!f.asc.requests.some((request) => request.method === "PATCH"), "nothing is committed after a failed part");
});

test("a FAILED build upload reports App Store Connect's reason and leaves no marker; a build upload that never completes times out", async () => {
  const failed = fixture({ fake: { uploadStates: ["PROCESSING", "FAILED"], uploadErrors: [{ code: "90000", description: "Invalid binary Bearer abcdefghijklmnopqrstuv" }] } });
  await assert.rejects(runIosUpload({ ...failed.deps, apply: true }), (error) => error.code === "IOS_BUILD_UPLOAD_FAILED" && /Invalid binary/.test(error.message) && !error.message.includes("abcdefghijklmnopqrstuv"));
  assert.ok(!existsSync(path.join(failed.stateRoot, IOS_UPLOADED_MARKER)));
  const stuck = fixture({ fake: { uploadStates: ["PROCESSING"] } });
  await assert.rejects(runIosUpload({ ...stuck.deps, apply: true }), (error) => error.code === "TESTFLIGHT_TIMEOUT");
  assert.ok(!existsSync(path.join(stuck.stateRoot, IOS_UPLOADED_MARKER)));
});

test("a TestFlight failure after a good upload keeps the upload marker, so a rerun does not upload twice", async () => {
  const f = fixture({ asc: fakeAsc({ failStatus: 403 }) });
  await assert.rejects(runIosUpload({ ...f.deps, apply: true }), (error) => error.code === "TESTFLIGHT_API_FAILED");
  assert.ok(existsSync(path.join(f.stateRoot, IOS_UPLOADED_MARKER)));
  assert.ok(!existsSync(path.join(f.stateRoot, IOS_DISTRIBUTED_MARKER)));
  const retry = fixture();
  const result = await runIosUpload({ ...f.deps, apply: true, fetchImpl: retry.asc.fetchImpl });
  assert.equal(result.applied, true);
  assert.ok(f.logs.some((line) => /already uploaded/.test(line)));
  assert.ok(!retry.asc.requests.some((request) => request.path.startsWith("/v1/buildUpload")), "the rerun does not upload again");
});

test("changed sealed bytes are refused before anything is sent", async () => {
  const f = fixture();
  writeFileSync(f.auth.ipa.file, "different bytes entirely");
  await assert.rejects(runIosUpload({ ...f.deps, apply: true }), (error) => error.code === "PUBLICATION_SOURCE_CHANGED" || error.code === "PUBLICATION_HASH_MISMATCH" || error.code === "IOS_IPA_CHANGED");
  assert.equal(f.asc.requests.length, 0);
});

test("a missing Keychain item stops the upload before the network", async () => {
  const f = fixture({ readCredentials: () => { const error = new Error("the key is not in the login Keychain"); error.code = "IOS_ASC_CREDENTIAL_MISSING"; throw error; } });
  await assert.rejects(runIosUpload({ ...f.deps, apply: true }), (error) => error.code === "IOS_ASC_CREDENTIAL_MISSING");
  assert.equal(f.fetchCalls.length, 0);
});

test("the plan names the sealed IPA by digest and never contains a key", () => {
  const f = fixture();
  const plan = iosUploadPlan({ distribution: f.auth.distribution, ipa: f.auth.entry, receipt: f.auth.receipt });
  assert.equal(plan.ipa.sha256, f.auth.entry.sha256);
  assert.ok(!JSON.stringify(plan).match(/PRIVATE KEY|Bearer/));
  assert.equal(plan.steps[0].id, "buildUploads.find");
});

test("authorize needs a sealed release id", async () => {
  await assert.rejects(authorizeIosUpload({ releaseId: "" }), (error) => error instanceof IosUploadError && error.code === "IOS_UPLOAD_RELEASE_REQUIRED");
  await assert.rejects(authorizeIosUpload({ releaseId: "../escape" }), (error) => error.code === "IOS_UPLOAD_RELEASE_REQUIRED");
});

// ---- P1/P2 hardening ----------------------------------------------------------------------------------------------
test("a marker written for another IPA, app or build number is refused instead of skipping the upload", async () => {
  const written = fixture();
  await runIosUpload({ ...written.deps, apply: true });
  const marker = path.join(written.stateRoot, IOS_DISTRIBUTED_MARKER);
  const original = JSON.parse(readFileSync(marker, "utf8"));
  for (const [key, value] of [["ipaSha256", "f".repeat(64)], ["appId", "1"], ["buildNumber", "1999010101"], ["marketingVersion", "0.0.1"], ["bundleId", "x.y"]]) {
    writeFileSync(marker, JSON.stringify({ ...original, [key]: value }));
    await assert.rejects(runIosUpload({ ...written.deps, apply: true, readCredentials: () => assert.fail("refused before any credential read") }), (error) => error.code === "IOS_UPLOAD_MARKER_MISMATCH" && error.message.includes(key), key);
  }
  writeFileSync(marker, "{ not json");
  await assert.rejects(runIosUpload({ ...written.deps, apply: true }), (error) => error.code === "IOS_UPLOAD_MARKER_INVALID");
  // the same guard on the uploaded marker (the TestFlight-only rerun path)
  fs.rmSync(marker);
  const uploaded = path.join(written.stateRoot, IOS_UPLOADED_MARKER);
  writeFileSync(uploaded, JSON.stringify({ ...JSON.parse(readFileSync(uploaded, "utf8")), ipaSha256: "e".repeat(64) }));
  await assert.rejects(runIosUpload({ ...written.deps, apply: true }), (error) => error.code === "IOS_UPLOAD_MARKER_MISMATCH");
  assert.equal(written.asc.puts.length, 3, "nothing was sent again");
});

test("a second apply of the same release is refused while the first holds the lock, and the lock is released afterwards", async () => {
  const f = fixture();
  const lockFile = path.join(f.repoRoot, ".right-release", "locks", `ios-upload-${RELEASE}.lock.json`);
  mkdirSync(path.dirname(lockFile), { recursive: true });
  writeFileSync(lockFile, JSON.stringify({ pid: process.ppid || 1, label: "other" }));
  const held = { ...f.deps, apply: true };
  // a live holder (this very process is excluded by design, so use an injected lock that behaves like a live foreign pid)
  const { acquireFileLock } = await import("./ios-lock.mjs");
  await assert.rejects(runIosUpload({ ...held, acquireLock: (file, label) => acquireFileLock(file, label, { isAlive: () => true, pid: 4242 }), readCredentials: () => assert.fail("locked out before the Keychain") }), (error) => error.code === "IOS_UPLOAD_LOCKED");
  assert.equal(f.asc.requests.length, 0);
  fs.rmSync(lockFile);
  await runIosUpload({ ...held });
  assert.ok(!existsSync(lockFile), "released after the run");
  await assert.rejects(runIosUpload({ ...held, authorize: async () => { throw Object.assign(new Error("boom"), { code: "X" }); } }));
  assert.ok(!existsSync(lockFile));
  // released on failure too
  const failing = fixture({ fake: { failPut: true } });
  const failingLock = path.join(failing.repoRoot, ".right-release", "locks", `ios-upload-${RELEASE}.lock.json`);
  await assert.rejects(runIosUpload({ ...failing.deps, apply: true }));
  assert.ok(!existsSync(failingLock), "released after a failed run");
});

test("the Keychain is read only in the GUI session: an SSH session is refused before the read and the network", async () => {
  const f = fixture();
  await assert.rejects(runIosUpload({ ...f.deps, apply: true, detectSession: () => ({ guiRunner: true, reason: "SSH session" }), readCredentials: () => assert.fail("no Keychain read over SSH") }), (error) => error.code === "IOS_GUI_SESSION_REQUIRED");
  assert.equal(f.fetchCalls.length, 0);
  const unknown = fixture();
  await assert.rejects(runIosUpload({ ...unknown.deps, apply: true, detectSession: () => { throw new Error("no launchctl"); }, readCredentials: () => assert.fail("unproven session") }), (error) => error.code === "IOS_GUI_SESSION_UNKNOWN");
});

test("the sealed IPA reaches the uploader as a file, never as bytes in memory", async () => {
  const f = fixture();
  assert.ok(typeof f.auth.ipa.file === "string");
  await runIosUpload({ ...f.deps, apply: true });
  assert.equal(Buffer.concat(f.asc.puts.map((put) => put.body)).toString(), "IPA-BYTES");
});

test("the upload journals the build upload it created in the vault state root; a foreign record for the same build is refused, and a corrupt journal is an error", async () => {
  const vault = tempRoot("rk-ios-vault-");
  const f = fixture();
  await runIosUpload({ ...f.deps, env: { ...f.deps.env, RIGHT_RELEASE_VAULT_STATE_ROOT: vault }, apply: true });
  const journalFile = path.join(vault, "ios-build-uploads", `ios-build-uploads.${APP_ID}.json`);
  const journal = JSON.parse(readFileSync(journalFile, "utf8"));
  assert.deepEqual(journal.entries.map((entry) => [entry.buildUploadId, entry.buildUploadFileId, entry.ipaSha256, entry.appId, entry.version, entry.build]), [["bu1", "bf1", f.auth.entry.sha256, APP_ID, "1.0.214", "2026101801"]]);
  assert.ok(!fs.readdirSync(path.dirname(journalFile)).some((name) => name.includes(".tmp-")), "atomic: no temporary file left");
  assert.ok(!existsSync(path.join(f.repoRoot, ".right-release", "state", "ios-build-uploads")), "not in the repository when a vault root is given");

  // a fresh vault (another Mac) meets the COMPLETE record of the first: refused, and nothing is sent
  const other = fixture({ fake: { upload: { existing: [{ id: "bu1", state: "COMPLETE" }] } } });
  await assert.rejects(runIosUpload({ ...other.deps, env: { ...other.deps.env, RIGHT_RELEASE_VAULT_STATE_ROOT: tempRoot("rk-ios-vault2-") }, apply: true }), (error) => error.code === "IOS_BUILD_UPLOAD_FOREIGN");
  assert.equal(other.asc.puts.length, 0);

  // the same vault resumes its own record
  const again = fixture({ fake: { upload: { existing: [{ id: "bu1", state: "COMPLETE" }] } } });
  const resumed = await runIosUpload({ ...again.deps, env: { ...again.deps.env, RIGHT_RELEASE_VAULT_STATE_ROOT: vault }, apply: true });
  assert.equal(resumed.applied, true);
  assert.equal(again.asc.puts.length, 0);

  writeFileSync(journalFile, "{broken");
  const corrupt = fixture();
  await assert.rejects(runIosUpload({ ...corrupt.deps, env: { ...corrupt.deps.env, RIGHT_RELEASE_VAULT_STATE_ROOT: vault }, apply: true }), (error) => error.code === "IOS_UPLOAD_JOURNAL_INVALID");
  assert.equal(corrupt.asc.requests.length, 0);
  const direct = buildUploadJournal(path.dirname(journalFile), APP_ID);
  writeFileSync(journalFile, JSON.stringify({ entries: [{ buildUploadId: "x", ipaSha256: "short" }] }));
  assert.throws(() => direct.read(), (error) => error.code === "IOS_UPLOAD_JOURNAL_INVALID");
  fs.rmSync(journalFile);
  assert.deepEqual(direct.read(), []);
});
