import assert from "node:assert/strict";
import { generateKeyPairSync, verify } from "node:crypto";
import test from "node:test";
import { fakeAsc } from "./test-support/fake-asc.mjs";
import { ASC_API, MAX_JWT_LIFETIME_SECONDS, RETRY_DEFAULTS, TestFlightError, createAscClient, createTokenProvider, distributeToTestFlight, normalizeGroupSelection, parseRetryAfter, retryDelayMs, testflightPlan } from "./ios-testflight.mjs";

const APP = "6806854037";
const MARKETING = "1.0.214";
const BUILD = "2026101801";
const { privateKey, publicKey } = generateKeyPairSync("ec", { namedCurve: "prime256v1" });
const coded = async (code, promise) => assert.rejects(promise, (error) => error instanceof TestFlightError && error.code === code, code);

const clientFor = (fake) => createAscClient({ tokenProvider: { get: () => "jwt-fixture" }, fetchImpl: fake.fetchImpl });
const options = (fake, extra = {}) => {
  let clock = 0;
  const sleeps = [];
  return { client: clientFor(fake), appId: APP, marketingVersion: MARKETING, buildNumber: BUILD, groups: "all", sleep: async (ms) => { sleeps.push(ms); clock += ms; }, monotonic: () => clock, pollIntervalMs: 1000, timeoutMs: 10_000, sleeps, ...extra };
};

test("distribute waits for the exact build, assigns every group and creates the external review", async () => {
  const fake = fakeAsc({ builds: [{ id: "b1", version: BUILD, marketing: MARKETING, states: ["PROCESSING", "PROCESSING", "VALID"] }] });
  const input = options(fake, { submitExternalBetaReview: true });
  const evidence = await distributeToTestFlight(input);
  assert.deepEqual(input.sleeps, [1000, 1000]);
  assert.equal(evidence.buildId, "b1");
  assert.equal(evidence.processingState, "VALID");
  assert.deepEqual(evidence.groups.map((group) => [group.name, group.assigned]), [["Internal Dogfood", true], ["Friends", true]]);
  assert.equal(evidence.externalBuildState, "WAITING_FOR_BETA_REVIEW");
  assert.deepEqual(evidence.betaReviewSubmissionIds, ["sub1"]);
  assert.equal(fake.requests.filter((request) => request.method === "POST" && request.path.includes("/relationships/builds")).length, 2);
  assert.ok(fake.requests.every((request) => request.path.startsWith("/v1/")));
});

test("a group that already holds the build is not assigned again; internal-only skips the external review", async () => {
  const fake = fakeAsc({ groups: [{ id: "g1", name: "Internal Dogfood", internal: true }], assigned: { g1: ["b1"] } });
  const evidence = await distributeToTestFlight(options(fake));
  assert.equal(fake.requests.filter((request) => request.method === "POST").length, 0);
  assert.equal(evidence.externalBuildState, null);
  assert.deepEqual(evidence.betaReviewSubmissionIds, []);
});

test("submitExternalBetaReview false leaves external review alone", async () => {
  const fake = fakeAsc();
  const evidence = await distributeToTestFlight(options(fake, { submitExternalBetaReview: false }));
  assert.equal(fake.requests.some((request) => request.path === "/v1/betaAppReviewSubmissions"), false);
  assert.equal(evidence.externalBuildState, null);
});

test("named groups: only those are assigned; an unknown name fails", async () => {
  const fake = fakeAsc();
  const evidence = await distributeToTestFlight(options(fake, { groups: ["Internal Dogfood"] }));
  assert.deepEqual(evidence.groups.map((group) => group.name), ["Internal Dogfood"]);
  assert.equal(fake.state.assigned.has("g2"), false);
  await coded("TESTFLIGHT_GROUP_MISSING", distributeToTestFlight(options(fakeAsc(), { groups: ["Nobody"] })));
  assert.throws(() => normalizeGroupSelection([]), (error) => error.code === "TESTFLIGHT_CONFIG_INVALID");
});

test("only the exact marketing version and build number match; two matches are ambiguous; a failed build stops", async () => {
  const other = fakeAsc({ builds: [{ id: "b9", version: BUILD, marketing: "9.9.9", states: ["VALID"] }] });
  await coded("TESTFLIGHT_TIMEOUT", distributeToTestFlight(options(other, { timeoutMs: 3000 })));
  const twice = fakeAsc({ builds: [{ id: "b1", version: BUILD, marketing: MARKETING, states: ["VALID"] }, { id: "b2", version: BUILD, marketing: MARKETING, states: ["VALID"] }] });
  await coded("TESTFLIGHT_AMBIGUOUS_BUILD", distributeToTestFlight(options(twice)));
  const failed = fakeAsc({ builds: [{ id: "b1", version: BUILD, marketing: MARKETING, states: ["FAILED"] }] });
  await coded("TESTFLIGHT_PROCESSING_FAILED", distributeToTestFlight(options(failed)));
});

test("a build that never finishes times out on the injected clock without a real wait", async () => {
  const slow = fakeAsc({ builds: [{ id: "b1", version: BUILD, marketing: MARKETING, states: ["PROCESSING"] }] });
  const input = options(slow, { timeoutMs: 5000 });
  await coded("TESTFLIGHT_TIMEOUT", distributeToTestFlight(input));
  assert.equal(input.sleeps.length, 5);
});

test("an assignment that keeps failing, with the group still empty, fails; an API error never leaks a token", async () => {
  await coded("TESTFLIGHT_ASSIGN_FAILED", distributeToTestFlight(options(fakeAsc({ failAssign: true }))));
  const denied = fakeAsc({ failStatus: 403 });
  await assert.rejects(distributeToTestFlight(options(denied)), (error) => {
    assert.equal(error.code, "TESTFLIGHT_API_FAILED");
    assert.match(error.message, /HTTP 403/);
    assert.ok(!error.message.includes("x".repeat(40)));
    assert.ok(!error.message.includes("jwt-fixture"));
    return true;
  });
});

test("an external build already in review is accepted without a second submission; a rejected review state fails", async () => {
  const waiting = fakeAsc({ externalState: "WAITING_FOR_BETA_REVIEW", submissions: [{ id: "s0", attributes: { betaReviewState: "WAITING_FOR_REVIEW" } }] });
  const evidence = await distributeToTestFlight(options(waiting, { submitExternalBetaReview: true }));
  assert.deepEqual(evidence.betaReviewSubmissionIds, ["s0"]);
  assert.equal(waiting.requests.filter((request) => request.method === "POST" && request.path === "/v1/betaAppReviewSubmissions").length, 0);
  const rejected = fakeAsc({ submissions: [{ id: "s0", attributes: { betaReviewState: "REJECTED" } }] });
  await coded("TESTFLIGHT_REVIEW_STATE", distributeToTestFlight(options(rejected, { submitExternalBetaReview: true })));
});

test("the client stays on the API origin: a foreign pagination link and a non-API path are refused", async () => {
  const foreign = createAscClient({ tokenProvider: { get: () => "t" }, fetchImpl: async () => ({ ok: true, status: 200, json: async () => ({ data: [], links: { next: "https://evil.example/v1/x" } }) }) });
  await coded("TESTFLIGHT_API_INVALID", foreign.getAll("/v1/apps/1/betaGroups"));
  await coded("TESTFLIGHT_API_INVALID", foreign.request("GET", "/etc/passwd"));
  const loop = createAscClient({ tokenProvider: { get: () => "t" }, fetchImpl: async () => ({ ok: true, status: 200, json: async () => ({ data: [], links: { next: `${ASC_API}/v1/apps/1/betaGroups` } }) }) });
  await coded("TESTFLIGHT_API_INVALID", loop.getAll("/v1/apps/1/betaGroups"));
});

test("the JWT is ES256 over the Keychain key in memory, expires in 18 minutes and is renewed before it ends", () => {
  let current = new Date("2026-10-18T10:00:00Z");
  const provider = createTokenProvider({ privateKey, keyId: "ABC123DEFG", issuer: "69a6de80-0b1c-47e3-e053-5b8c7c11a4d1", now: () => current });
  const first = provider.get();
  const [head, claims, signature] = first.split(".");
  assert.deepEqual(JSON.parse(Buffer.from(head, "base64url")), { alg: "ES256", kid: "ABC123DEFG", typ: "JWT" });
  const payload = JSON.parse(Buffer.from(claims, "base64url"));
  assert.equal(payload.aud, "appstoreconnect-v1");
  assert.equal(payload.exp - payload.iat, 1080);
  assert.ok(MAX_JWT_LIFETIME_SECONDS <= 1080);
  assert.equal(verify("sha256", Buffer.from(`${head}.${claims}`), { key: publicKey, dsaEncoding: "ieee-p1363" }, Buffer.from(signature, "base64url")), true);
  current = new Date(current.getTime() + 600_000);
  assert.equal(provider.get(), first, "reused while valid");
  current = new Date(current.getTime() + 425_000);
  assert.notEqual(provider.get(), first, "renewed within a minute of expiry");
});

test("the dry-run plan lists the API calls and has no credential in it", () => {
  const plan = testflightPlan({ appId: APP, marketingVersion: MARKETING, buildNumber: BUILD, groups: ["Internal Dogfood"], submitExternalBetaReview: true });
  assert.deepEqual(plan.map((step) => step.id), ["builds.wait", "betaGroups.list", "betaGroups.assign", "betaAppReviewSubmissions.create"]);
  assert.ok(!JSON.stringify(plan).match(/Bearer|PRIVATE|jwt/i));
});

test("the JWT lifetime is capped below Apple's 20 minute limit", () => {
  for (const lifetimeSeconds of [1081, 1200, 60, 0.5]) assert.throws(() => createTokenProvider({ privateKey, keyId: "K", issuer: "I", lifetimeSeconds }), (error) => error.code === "TESTFLIGHT_CONFIG_INVALID");
});

test("no policy is implicit: groups is required and external beta review is off unless asked for", async () => {
  assert.throws(() => normalizeGroupSelection(undefined), (error) => error.code === "TESTFLIGHT_CONFIG_INVALID" && /groups is required/.test(error.message));
  const fake = fakeAsc();
  const input = options(fake);
  delete input.groups;
  await coded("TESTFLIGHT_CONFIG_INVALID", distributeToTestFlight(input));
  const evidence = await distributeToTestFlight(options(fake));
  assert.equal(evidence.externalBuildState, null, "an external group is assigned but no beta review is created by default");
  assert.equal(fake.requests.some((request) => request.path === "/v1/betaAppReviewSubmissions"), false);
});

// ---- bounded retry ---------------------------------------------------------------------------------------------------
const reply = (status, body = {}, headers = {}) => ({ ok: status < 400, status, json: async () => body, headers: { get: (name) => headers[name.toLowerCase()] ?? null } });
function scripted(responses) {
  const calls = [];
  const sleeps = [];
  const fetchImpl = async (url, init) => { calls.push({ method: init.method, url }); const next = responses.shift(); if (next instanceof Error) throw next; return next; };
  return { calls, sleeps, client: createAscClient({ tokenProvider: { get: () => "t" }, fetchImpl, sleep: async (ms) => { sleeps.push(ms); } }) };
}

test("GET and PATCH are retried on 429, 5xx and network errors with bounded backoff, honouring Retry-After", async () => {
  const get = scripted([reply(503), new Error("socket hang up"), reply(429, {}, { "retry-after": "7" }), reply(200, { data: [] })]);
  assert.deepEqual(await get.client.request("GET", "/v1/builds"), { data: [] });
  assert.equal(get.calls.length, 4);
  assert.deepEqual(get.sleeps, [1000, 2000, 7000]);
  const patch = scripted([reply(500), reply(200, { ok: 1 })]);
  assert.deepEqual(await patch.client.request("PATCH", "/v1/buildUploadFiles/f", { data: {} }), { ok: 1 });
  assert.equal(patch.calls.length, 2);
});

test("a create POST is never retried, and other 4xx are not retried at all", async () => {
  const post = scripted([reply(503), reply(200, { data: { id: "late" } })]);
  await coded("TESTFLIGHT_API_FAILED", post.client.request("POST", "/v1/buildUploads", { data: {} }));
  assert.equal(post.calls.length, 1);
  assert.deepEqual(post.sleeps, []);
  const forbidden = scripted([reply(403), reply(200)]);
  await coded("TESTFLIGHT_API_FAILED", forbidden.client.request("GET", "/v1/builds"));
  assert.equal(forbidden.calls.length, 1);
});

test("retries are bounded: the attempts run out, Retry-After and backoff are capped", async () => {
  const always = scripted(Array.from({ length: 10 }, () => reply(503, {}, { "retry-after": "86400" })));
  await coded("TESTFLIGHT_API_FAILED", always.client.request("GET", "/v1/builds"));
  assert.equal(always.calls.length, RETRY_DEFAULTS.attempts);
  assert.ok(always.sleeps.every((ms) => ms === RETRY_DEFAULTS.maxRetryAfterMs));
  assert.equal(retryDelayMs(10, undefined), RETRY_DEFAULTS.maxBackoffMs);
  assert.equal(retryDelayMs(0, 500), 500);
  assert.equal(parseRetryAfter("3"), 3000);
  assert.equal(parseRetryAfter("Wed, 21 Oct 2026 07:28:10 GMT", Date.parse("Wed, 21 Oct 2026 07:28:00 GMT")), 10_000);
  assert.equal(parseRetryAfter("soon"), undefined);
});
