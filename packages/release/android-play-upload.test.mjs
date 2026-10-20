import assert from "node:assert/strict";
import { generateKeyPairSync, createVerify } from "node:crypto";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { scanAabBuffer } from "./android-aab-scan.mjs";
import { FIXTURE_CERT_SHA256, aab, fakeCertificate, markedLib } from "./android-test-fixtures.mjs";
import {
  PLAY_API, PLAY_MARKER_NAME, PLAY_TOKEN_URL, PLAY_UPLOAD_API, PlayUploadError, assertUploadableScan, fetchAccessToken, parseServiceAccount,
  playEnvSecretNames, playUploadPlan, refusePlayEnvSecrets, runPlayUpload, signServiceAccountAssertion, uploadToPlay, validatePlayDistribution,
} from "./android-play-upload.mjs";
import { digestBytes, readVerifiedArtifact } from "./publication-artifacts.mjs";

const PACKAGE = "com.orthiclabs.heardright";
const { privateKey: PEM, publicKey } = generateKeyPairSync("rsa", { modulusLength: 2048, privateKeyEncoding: { type: "pkcs8", format: "pem" }, publicKeyEncoding: { type: "spki", format: "pem" } });
const SERVICE_ACCOUNT = JSON.stringify({ type: "service_account", client_email: "uploader@heardright.iam.gserviceaccount.com", private_key: PEM, token_uri: PLAY_TOKEN_URL });
const ACCESS_TOKEN = "ya29.fixture-access-token";
const distribution = (over = {}) => ({ provider: "google-play", packageName: PACKAGE, serviceAccount: { credential: "play-service-account" }, ...over });
const refused = (code, fn) => assert.throws(fn, (error) => error instanceof PlayUploadError && error.code === code, code);

// A recording fake of the Google endpoints. `fail` names a step id that answers 403.
function fakePlay({ fail, versionCode = 42 } = {}) {
  const requests = [];
  const respond = (status, body) => ({ ok: status < 400, status, json: async () => body });
  const fetchImpl = async (url, init = {}) => {
    const request = { url, method: init.method ?? "GET", headers: init.headers ?? {}, body: init.body };
    requests.push(request);
    const step = url === PLAY_TOKEN_URL ? "token" : request.method === "DELETE" ? "delete"
      : url.includes("/bundles") ? "bundles.upload" : url.includes("/tracks/") ? "tracks.update" : url.endsWith(":commit") ? "edits.commit" : "edits.insert";
    request.step = step;
    if (fail === step) return respond(403, { error: { message: `permission denied for ${step} Bearer ${ACCESS_TOKEN}`.padEnd(900, "x") } });
    if (step === "token") return respond(200, { access_token: ACCESS_TOKEN, expires_in: 3600 });
    if (step === "edits.insert") return respond(200, { id: "edit-1", expiryTimeSeconds: "1" });
    if (step === "bundles.upload") return respond(200, { versionCode });
    if (step === "tracks.update") return respond(200, { track: "internal" });
    if (step === "edits.commit") return respond(200, { id: "edit-1" });
    return respond(200, {});
  };
  return { fetchImpl, requests };
}

test("distribution config: google-play, a production package, the internal track only, no inline secret", () => {
  const ok = validatePlayDistribution(distribution({ releaseName: "HeardRight 1.2.3" }));
  assert.equal(ok.track, "internal");
  assert.equal(ok.releaseStatus, "completed");
  assert.deepEqual(ok.serviceAccount, { credential: "play-service-account" });
  assert.equal(validatePlayDistribution(distribution({ releaseStatus: "draft", serviceAccount: { file: "~/keys/play.json" } })).releaseStatus, "draft");
  refused("PLAY_CONFIG_INVALID", () => validatePlayDistribution(undefined));
  refused("PLAY_CONFIG_INVALID", () => validatePlayDistribution(distribution({ provider: "github-releases" })));
  refused("PLAY_CONFIG_INVALID", () => validatePlayDistribution(distribution({ packageName: "heardright" })));
  refused("PLAY_TEST_PACKAGE", () => validatePlayDistribution(distribution({ packageName: `${PACKAGE}.dev` })));
  refused("PLAY_TEST_PACKAGE", () => validatePlayDistribution(distribution({ packageName: `${PACKAGE}.qa` })));
  refused("PLAY_TRACK_REFUSED", () => validatePlayDistribution(distribution({ track: "production" })));
  refused("PLAY_TRACK_REFUSED", () => validatePlayDistribution(distribution({ track: "alpha" })));
  refused("PLAY_CONFIG_INVALID", () => validatePlayDistribution(distribution({ releaseStatus: "inProgress" })));
  refused("PLAY_CONFIG_INVALID", () => validatePlayDistribution(distribution({ releaseName: "a\nb" })));
  refused("PLAY_CONFIG_INVALID", () => validatePlayDistribution(distribution({ serviceAccount: SERVICE_ACCOUNT })));
  refused("PLAY_CONFIG_INVALID", () => validatePlayDistribution(distribution({ serviceAccount: { json: SERVICE_ACCOUNT } })));
  refused("PLAY_CONFIG_INVALID", () => validatePlayDistribution(distribution({ serviceAccountJson: SERVICE_ACCOUNT })));
  refused("PLAY_CONFIG_INVALID", () => validatePlayDistribution({ ...distribution(), serviceAccount: undefined }));
});

test("key material in the ambient environment is refused by name, never by value", () => {
  assert.deepEqual(playEnvSecretNames({ PATH: "x", HOME: "y" }), []);
  assert.deepEqual(playEnvSecretNames({ PLAY_SERVICE_ACCOUNT_JSON: "{}" }), ["PLAY_SERVICE_ACCOUNT_JSON"]);
  assert.deepEqual(playEnvSecretNames({ ANYTHING: PEM }), ["ANYTHING"]);
  assert.deepEqual(playEnvSecretNames({ ANYTHING: SERVICE_ACCOUNT }), ["ANYTHING"]);
  assert.deepEqual(playEnvSecretNames({ PLAY_SERVICE_ACCOUNT_JSON: "   " }), []);
  try { refusePlayEnvSecrets({ ANYTHING: PEM }); assert.fail("expected refusal"); } catch (error) {
    assert.equal(error.code, "PLAY_ENV_SECRET_REFUSED");
    assert.ok(!error.message.includes("PRIVATE KEY"));
  }
});

test("service account parsing: a real key file shape, the Google token endpoint only", () => {
  const account = parseServiceAccount(SERVICE_ACCOUNT);
  assert.equal(account.clientEmail, "uploader@heardright.iam.gserviceaccount.com");
  refused("PLAY_SERVICE_ACCOUNT_INVALID", () => parseServiceAccount("not json"));
  refused("PLAY_SERVICE_ACCOUNT_INVALID", () => parseServiceAccount(JSON.stringify({ type: "authorized_user" })));
  refused("PLAY_SERVICE_ACCOUNT_INVALID", () => parseServiceAccount(JSON.stringify({ type: "service_account", client_email: "a@b", private_key: "nope" })));
  refused("PLAY_SERVICE_ACCOUNT_INVALID", () => parseServiceAccount(JSON.stringify({ ...JSON.parse(SERVICE_ACCOUNT), token_uri: "https://evil.example/token" })));
  try { parseServiceAccount("{\"private_key\": \"-----BEGIN " + "PRIVATE KEY-----leak\""); } catch (error) { assert.ok(!error.message.includes("leak")); }
});

test("the JWT assertion is RS256, scoped to androidpublisher, short-lived and verifies against the public key", () => {
  const assertion = signServiceAccountAssertion(parseServiceAccount(SERVICE_ACCOUNT), { nowSeconds: 1_700_000_000 });
  const [head, claims, signature] = assertion.split(".");
  assert.deepEqual(JSON.parse(Buffer.from(head, "base64url")), { alg: "RS256", typ: "JWT" });
  const body = JSON.parse(Buffer.from(claims, "base64url"));
  assert.deepEqual(body, { iss: "uploader@heardright.iam.gserviceaccount.com", scope: "https://www.googleapis.com/auth/androidpublisher", aud: PLAY_TOKEN_URL, iat: 1_700_000_000, exp: 1_700_003_600 });
  assert.equal(createVerify("RSA-SHA256").update(`${head}.${claims}`).verify(publicKey, Buffer.from(signature, "base64url")), true);
});

test("the token exchange posts the assertion as a form body and returns only the access token", async () => {
  const play = fakePlay();
  const token = await fetchAccessToken(parseServiceAccount(SERVICE_ACCOUNT), { fetchImpl: play.fetchImpl, sign: () => Buffer.from("sig") });
  assert.equal(token, ACCESS_TOKEN);
  const [request] = play.requests;
  assert.equal(request.url, PLAY_TOKEN_URL);
  assert.equal(request.method, "POST");
  const form = new URLSearchParams(request.body);
  assert.equal(form.get("grant_type"), "urn:ietf:params:oauth:grant-type:jwt-bearer");
  assert.match(form.get("assertion"), /^[\w-]+\.[\w-]+\.[\w-]+$/);
  assert.ok(!request.body.includes("PRIVATE KEY"));
  await assert.rejects(fetchAccessToken(parseServiceAccount(SERVICE_ACCOUNT), { fetchImpl: fakePlay({ fail: "token" }).fetchImpl, sign: () => Buffer.from("sig") }), /Play OAuth token exchange failed with HTTP 403/);
});

test("the plan lists the four edit calls in order and carries no secret", () => {
  const plan = playUploadPlan({ distribution: validatePlayDistribution(distribution({ releaseName: "R" })), aab: { name: "app.aab", sha256: "a".repeat(64), sizeBytes: 9 } });
  assert.deepEqual(plan.steps.map((step) => step.id), ["edits.insert", "bundles.upload", "tracks.update", "edits.commit"]);
  assert.deepEqual(plan.steps.map((step) => step.method), ["POST", "POST", "PUT", "POST"]);
  assert.equal(plan.steps[2].body.track, "internal");
  assert.equal(plan.steps[2].body.releases[0].status, "completed");
  assert.ok(plan.steps[2].url.endsWith("/tracks/internal"));
  assert.ok(!JSON.stringify(plan).match(/Bearer|private_key|access_token/));
});

test("uploadToPlay runs insert, bundle upload, internal track update and commit with a bearer token", async () => {
  const play = fakePlay({ versionCode: 77 });
  const bytes = Buffer.from("aab bytes");
  const logs = [];
  const result = await uploadToPlay({ distribution: validatePlayDistribution(distribution({ releaseName: "HeardRight 1.2.3" })), aabBytes: bytes, accessToken: ACCESS_TOKEN, fetchImpl: play.fetchImpl, log: (line) => logs.push(line) });
  assert.deepEqual(result, { editId: "edit-1", versionCode: 77 });
  assert.deepEqual(play.requests.map((request) => request.step), ["edits.insert", "bundles.upload", "tracks.update", "edits.commit"]);
  assert.equal(play.requests[0].url, `${PLAY_API}/${PACKAGE}/edits`);
  assert.equal(play.requests[1].url, `${PLAY_UPLOAD_API}/${PACKAGE}/edits/edit-1/bundles?uploadType=media`);
  assert.equal(play.requests[1].headers["content-type"], "application/octet-stream");
  assert.equal(play.requests[1].body, bytes);
  assert.equal(play.requests[2].method, "PUT");
  assert.equal(play.requests[2].url, `${PLAY_API}/${PACKAGE}/edits/edit-1/tracks/internal`);
  assert.deepEqual(JSON.parse(play.requests[2].body), { track: "internal", releases: [{ name: "HeardRight 1.2.3", status: "completed", versionCodes: ["77"] }] });
  assert.equal(play.requests[3].url, `${PLAY_API}/${PACKAGE}/edits/edit-1:commit`);
  for (const request of play.requests) assert.equal(request.headers.authorization, `Bearer ${ACCESS_TOKEN}`);
  assert.ok(!logs.join("\n").includes(ACCESS_TOKEN));
});

test("a failure after the edit opens deletes the edit, never commits, and keeps the token out of the message", async () => {
  for (const step of ["bundles.upload", "tracks.update", "edits.commit"]) {
    const play = fakePlay({ fail: step });
    await assert.rejects(
      uploadToPlay({ distribution: validatePlayDistribution(distribution()), aabBytes: Buffer.from("x"), accessToken: ACCESS_TOKEN, fetchImpl: play.fetchImpl, log: () => {} }),
      (error) => error.code === "PLAY_API_FAILED" && error.message.includes(`${step} failed with HTTP 403`) && error.message.length < 400 && !error.message.includes(ACCESS_TOKEN),
    );
    const last = play.requests.at(-1);
    assert.equal(last.method, "DELETE");
    assert.equal(last.url, `${PLAY_API}/${PACKAGE}/edits/edit-1`);
    if (step !== "edits.commit") assert.ok(!play.requests.some((request) => request.step === "edits.commit"));
  }
  const early = fakePlay({ fail: "edits.insert" });
  await assert.rejects(uploadToPlay({ distribution: validatePlayDistribution(distribution()), aabBytes: Buffer.from("x"), accessToken: ACCESS_TOKEN, fetchImpl: early.fetchImpl, log: () => {} }), /edits.insert failed/);
  assert.equal(early.requests.length, 1);
});

test("a debug-signed, unsigned, unpinned or marked bundle is refused for upload", () => {
  const scan = (bytes) => scanAabBuffer(bytes);
  assert.doesNotThrow(() => assertUploadableScan(scan(aab()), FIXTURE_CERT_SHA256));
  refused("PLAY_AAB_DEBUG_SIGNED", () => assertUploadableScan(scan(aab({ certificate: fakeCertificate("CN=Android Debug,O=Android,C=US") })), FIXTURE_CERT_SHA256));
  assert.throws(() => assertUploadableScan(scan(aab({ signed: false })), FIXTURE_CERT_SHA256), (error) => /^PLAY_AAB_(?:REFUSED|UNSIGNED)$/.test(error.code));
  refused("PLAY_AAB_SIGNER_MISMATCH", () => assertUploadableScan(scan(aab()), "b".repeat(64)));
  refused("PLAY_AAB_SIGNER_MISMATCH", () => assertUploadableScan(scan(aab()), undefined));
  refused("PLAY_AAB_REFUSED", () => assertUploadableScan(scan(aab({ override: { "base/lib/arm64-v8a/libapp.so": markedLib() } })), FIXTURE_CERT_SHA256));
});

// ---- runPlayUpload ------------------------------------------------------------------------------------------------
function sealedFixture(t) {
  const base = mkdtempSync(path.join(tmpdir(), "rk-play-"));
  t.after(() => rmSync(base, { recursive: true, force: true }));
  const repoRoot = path.join(base, "repo");
  mkdirSync(repoRoot, { recursive: true });
  const file = path.join(repoRoot, "app-release.aab");
  const bytes = aab();
  writeFileSync(file, bytes);
  const entry = { role: "aab", name: "app-release.aab", sha256: digestBytes(bytes), sizeBytes: bytes.length };
  const artifact = readVerifiedArtifact(file, entry);
  const authorize = async () => ({ repoRoot, configPath: path.join(repoRoot, "right-release.config.mjs"), distribution: validatePlayDistribution(distribution()), aab: artifact, entry });
  return { repoRoot, authorize, entry, bytes };
}
const storeOf = (log) => ({ read(name) { log.push(name); return SERVICE_ACCOUNT; } });

test("dry run is the default: the plan prints, no secret is read, no request is made", async (t) => {
  const fixture = sealedFixture(t);
  const reads = []; const logs = []; const play = fakePlay();
  const result = await runPlayUpload({ releaseId: "r1", authorize: fixture.authorize, env: {}, fetchImpl: play.fetchImpl, credentialStore: storeOf(reads), log: (line) => logs.push(line) });
  assert.equal(result.applied, false);
  assert.deepEqual(reads, []);
  assert.deepEqual(play.requests, []);
  assert.match(logs[0], /dry-run \(default; pass --apply to upload\)/);
  assert.ok(logs.join("\n").includes("bundles.upload"));
  assert.equal(existsSync(path.join(fixture.repoRoot, ".right-release", "state", "r1", "android", PLAY_MARKER_NAME)), false);
});

test("--apply reads the service account from the credential store, uploads once, and leaves no secret in any log", async (t) => {
  const fixture = sealedFixture(t);
  const reads = []; const logs = []; const play = fakePlay({ versionCode: 9 });
  const options = { releaseId: "r1", apply: true, authorize: fixture.authorize, env: {}, fetchImpl: play.fetchImpl, credentialStore: storeOf(reads), log: (line) => logs.push(line) };
  const result = await runPlayUpload(options);
  assert.equal(result.applied, true);
  assert.equal(result.versionCode, 9);
  assert.deepEqual(reads, ["play-service-account"]);
  assert.deepEqual(play.requests.map((request) => request.step), ["token", "edits.insert", "bundles.upload", "tracks.update", "edits.commit"]);
  assert.equal(play.requests[2].body.length, fixture.bytes.length);
  const everything = `${logs.join("\n")}\n${JSON.stringify(result)}`;
  assert.ok(!everything.includes(ACCESS_TOKEN));
  assert.ok(!everything.includes("PRIVATE KEY"));
  assert.ok(!everything.includes(PEM.trim().split("\n")[1]));
  const marker = JSON.parse(readFileSync(path.join(fixture.repoRoot, ".right-release", "state", "r1", "android", PLAY_MARKER_NAME), "utf8"));
  assert.equal(marker.versionCode, 9);
  assert.equal(marker.track, "internal");
  assert.equal(marker.aabSha256, fixture.entry.sha256);
  assert.ok(!JSON.stringify(marker).includes("PRIVATE KEY"));
  // The same sealed release is not uploaded twice.
  const again = await runPlayUpload({ ...options, fetchImpl: fakePlay().fetchImpl, log: () => {} });
  assert.equal(again.alreadyUploaded, true);
});

test("a failed apply writes no marker and surfaces only the step and the HTTP status", async (t) => {
  const fixture = sealedFixture(t);
  const play = fakePlay({ fail: "tracks.update" });
  await assert.rejects(runPlayUpload({ releaseId: "r1", apply: true, authorize: fixture.authorize, env: {}, fetchImpl: play.fetchImpl, credentialStore: storeOf([]), log: () => {} }), /tracks.update failed with HTTP 403/);
  assert.equal(existsSync(path.join(fixture.repoRoot, ".right-release", "state", "r1", "android", PLAY_MARKER_NAME)), false);
});

test("a changed AAB after verification is not uploaded, and key material in the environment stops the command before anything", async (t) => {
  const fixture = sealedFixture(t);
  const play = fakePlay();
  const tampered = async () => { const auth = await fixture.authorize(); writeFileSync(auth.aab.file, Buffer.concat([fixture.bytes, Buffer.from("x")])); return auth; };
  await assert.rejects(runPlayUpload({ releaseId: "r1", apply: true, authorize: tampered, env: {}, fetchImpl: play.fetchImpl, credentialStore: storeOf([]), log: () => {} }), /replaced after verification|bytes changed/);
  assert.deepEqual(play.requests, []);
  let authorized = false;
  await assert.rejects(runPlayUpload({ releaseId: "r1", apply: true, env: { PLAY_SERVICE_ACCOUNT_JSON: SERVICE_ACCOUNT }, authorize: async () => { authorized = true; }, fetchImpl: play.fetchImpl, log: () => {} }), /PLAY_ENV_SECRET_REFUSED/);
  assert.equal(authorized, false);
});
