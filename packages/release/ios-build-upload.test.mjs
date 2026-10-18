import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { writeFileSync } from "node:fs";
import path from "node:path";
import test from "node:test";
import { UPLOAD_HOST_SUFFIXES, assertUploadOperation, buildUploadPlan, fileSource, uploadIpaToAppStoreConnect } from "./ios-build-upload.mjs";
import { createAscClient } from "./ios-testflight.mjs";
import { fakeAsc } from "./test-support/fake-asc.mjs";
import { tempRoot } from "./test-support/temp-roots.mjs";

const BYTES = Buffer.from("0123456789");
const NOW = Date.parse("2026-10-18T10:00:00Z");
const IDENTITY = { appId: "6806854037", version: "1.0.214", build: "2026101801", ipaSha256: createHash("sha256").update(BYTES).digest("hex") };
const ours = (buildUploadId, over = {}) => ({ buildUploadId, buildUploadFileId: null, ...IDENTITY, ...over });
function memoryJournal(entries = []) {
  const list = [...entries];
  return { list, read: () => list.map((entry) => ({ ...entry })), record: (entry) => { const i = list.findIndex((item) => item.buildUploadId === entry.buildUploadId); if (i >= 0) list[i] = entry; else list.push(entry); } };
}
const coded = (code, promise) => assert.rejects(promise, (error) => error.code === code, code);

function sealed(bytes = BYTES) {
  const file = path.join(tempRoot("rk-ios-bu-"), "A.ipa");
  writeFileSync(file, bytes);
  return fileSource(file, { sha256: createHash("sha256").update(bytes).digest("hex"), sizeBytes: bytes.length });
}
function setup(fakeOptions = {}, extra = {}) {
  const fake = fakeAsc(fakeOptions);
  const sleeps = [];
  let clock = 0;
  const sleep = async (ms) => { sleeps.push(ms); clock += ms; };
  const source = extra.source ?? sealed();
  const reads = [];
  const journal = extra.journal ?? memoryJournal(extra.entries);
  const spied = { ...source, read: (offset, length) => { reads.push([offset, length]); return source.read(offset, length); } };
  return {
    fake, sleeps, reads, source, journal,
    run: () => uploadIpaToAppStoreConnect({
      client: createAscClient({ tokenProvider: { get: () => "jwt" }, fetchImpl: fake.fetchImpl, sleep }), appId: "6806854037", marketingVersion: "1.0.214", buildNumber: "2026101801",
      source: spied, fileName: "A.ipa", journal, fetchImpl: fake.fetchImpl, sleep, monotonic: () => clock, now: extra.now ?? (() => NOW), pollIntervalMs: 1000, timeoutMs: 5000, ...extra.options,
    }),
  };
}
const api = (fake) => fake.requests.filter((request) => request.path.startsWith("/v1/")).map((request) => `${request.method} ${request.path}`);

test("the sealed file is hashed in a stream and read in ranges: the parts tile the IPA, are sent in order, and no read is the whole file", async () => {
  const t = setup({ partSize: 3 });
  const result = await t.run();
  assert.equal(result.parts, 4);
  assert.equal(result.resumed, false);
  assert.equal(Buffer.concat(t.fake.puts.map((put) => put.body)).toString(), "0123456789");
  assert.deepEqual(t.reads, [[0, 3], [3, 3], [6, 3], [9, 1]]);
  assert.ok(t.fake.puts.every((put) => put.method === "PUT" && !("Authorization" in put.headers) && put.headers["Content-Type"] === "application/octet-stream"));
  assert.deepEqual(api(t.fake).slice(0, 3), ["GET /v1/apps/6806854037/buildUploads", "POST /v1/buildUploads", "POST /v1/buildUploadFiles"]);
  assert.deepEqual(t.journal.list, [{ buildUploadId: "bu1", buildUploadFileId: "bf1", ...IDENTITY }], "journalled after the create POSTs, with the sealed IPA sha256");
});

test("a source that changed after the seal is refused when it is opened, and again if it changes mid-upload", async () => {
  const dir = tempRoot("rk-ios-bu-");
  const file = path.join(dir, "A.ipa");
  writeFileSync(file, BYTES);
  const evidence = { sha256: createHash("sha256").update(BYTES).digest("hex"), sizeBytes: BYTES.length };
  const source = fileSource(file, evidence);
  writeFileSync(file, "0123456789-and-more");
  assert.throws(() => source.read(0, 3), (error) => error.code === "IOS_BUILD_UPLOAD_SOURCE_CHANGED");
  writeFileSync(file, "different!");
  assert.throws(() => fileSource(file, evidence), (error) => /mismatch/.test(error.code) || /PUBLICATION_/.test(error.code));
});

test("part PUTs are retried on 5xx and 429 with bounded backoff, honouring Retry-After; other statuses are not retried", async () => {
  const flaky = setup({ partSize: 5, upload: { putStatuses: [503, 429, 200, 200], retryAfter: "2" } });
  const result = await flaky.run();
  assert.equal(result.parts, 2);
  assert.equal(flaky.fake.puts.length, 4, "two refused attempts, then both parts");
  assert.deepEqual(flaky.sleeps.slice(0, 2), [2000, 2000]);
  const refused = setup({ partSize: 5, upload: { putStatuses: [403] } });
  await coded("IOS_BUILD_UPLOAD_FAILED", refused.run());
  assert.equal(refused.fake.puts.length, 1);
  assert.ok(!api(refused.fake).includes("PATCH /v1/buildUploadFiles/bf1"), "nothing is committed after a refused part");
  const exhausted = setup({ partSize: 5, upload: { putStatuses: Array(8).fill(503) } });
  await coded("IOS_BUILD_UPLOAD_FAILED", exhausted.run());
  assert.equal(exhausted.fake.puts.length, 5);
});

test("the commit and the state poll are retried too, but the create POSTs are not", async () => {
  const t = setup({ upload: { flaky: { "GET /v1/buildUploads/bu1": [503, 503], "PATCH /v1/buildUploadFiles/bf1": [500] } } });
  await t.run();
  assert.equal(t.fake.requests.filter((request) => request.method === "PATCH").length, 2);
  assert.equal(t.fake.requests.filter((request) => request.method === "GET" && request.path === "/v1/buildUploads/bu1").length >= 3, true);
  const post = setup({ upload: { flaky: { "POST /v1/buildUploads": [503] } } });
  await assert.rejects(post.run(), (error) => error.code === "TESTFLIGHT_API_FAILED");
  assert.equal(post.fake.requests.filter((request) => request.method === "POST" && request.path === "/v1/buildUploads").length, 1);
});

test("an existing AWAITING_UPLOAD build upload is resumed: no new create, the parts are sent again, then commit", async () => {
  const t = setup({ partSize: 5, upload: { existing: [{ id: "bu9", state: "AWAITING_UPLOAD", files: [{ id: "bf9", fileName: "A.ipa", fileSize: 10 }] }] } }, { entries: [ours("bu9", { buildUploadFileId: "bf9" })] });
  const result = await t.run();
  assert.equal(result.resumed, true);
  assert.equal(result.buildUploadId, "bu9");
  const calls = api(t.fake);
  assert.ok(!calls.includes("POST /v1/buildUploads") && !calls.includes("POST /v1/buildUploadFiles"));
  assert.ok(calls.includes("GET /v1/buildUploads/bu9/buildUploadFiles") && calls.includes("PATCH /v1/buildUploadFiles/bf9"));
  assert.equal(t.fake.puts.length, 2);
});

test("a resumed upload without a reservation reserves one (and still does not create a second build upload)", async () => {
  const t = setup({ partSize: 10, upload: { existing: [{ id: "bu9", state: "AWAITING_UPLOAD", files: [] }] } }, { entries: [ours("bu9")] });
  await t.run();
  const calls = api(t.fake);
  assert.ok(!calls.includes("POST /v1/buildUploads") && calls.includes("POST /v1/buildUploadFiles"));
});

test("an existing COMPLETE upload is finished without sending anything; PROCESSING is waited for; a different version or build is ignored", async () => {
  const done = setup({ upload: { existing: [{ id: "bu7", state: "COMPLETE" }] } }, { entries: [ours("bu7")] });
  const result = await done.run();
  assert.deepEqual([result.buildUploadId, result.resumed, result.parts], ["bu7", true, 0]);
  assert.deepEqual(api(done.fake), ["GET /v1/apps/6806854037/buildUploads", "GET /v1/buildUploads/bu7/buildUploadFiles"]);
  assert.equal(done.fake.puts.length, 0);
  const processing = setup({ upload: { existing: [{ id: "bu8", state: "PROCESSING" }] } }, { entries: [ours("bu8")] });
  await processing.run();
  assert.equal(processing.fake.puts.length, 0);
  assert.ok(!api(processing.fake).includes("POST /v1/buildUploads"));
  const other = setup({ upload: { existing: [{ id: "bu6", state: "COMPLETE", build: "1999" }, { id: "bu5", state: "FAILED", marketing: "0.0.1" }] } });
  assert.equal((await other.run()).resumed, false);
});

test("an existing FAILED upload for this version and build is refused clearly, with nothing created or sent", async () => {
  const t = setup({ upload: { existing: [{ id: "bu4", state: "FAILED" }] } });
  await assert.rejects(t.run(), (error) => error.code === "IOS_BUILD_UPLOAD_FAILED" && /FAILED build upload \(bu4\)/.test(error.message) && /new build number/.test(error.message));
  assert.deepEqual(api(t.fake), ["GET /v1/apps/6806854037/buildUploads"]);
  const ambiguous = setup({ upload: { existing: [{ id: "a", state: "AWAITING_UPLOAD" }, { id: "b", state: "PROCESSING" }] } }, { entries: [ours("a"), ours("b")] });
  await coded("IOS_BUILD_UPLOAD_AMBIGUOUS", ambiguous.run());
  const wrongSize = setup({ upload: { existing: [{ id: "c", state: "AWAITING_UPLOAD", files: [{ id: "f", fileName: "A.ipa", fileSize: 99 }] }] } }, { entries: [ours("c")] });
  await coded("IOS_BUILD_UPLOAD_MISMATCH", wrongSize.run());
});

test("an expired upload operation is refused before its PUT, from the operation or from the URL's Expires", async () => {
  const stale = setup({ upload: { expiration: "2026-10-18T09:00:00Z" } });
  await coded("IOS_BUILD_UPLOAD_EXPIRED", stale.run());
  assert.equal(stale.fake.puts.length, 0);
  const urlExpired = setup({ upload: { urlExpires: String(Math.floor(NOW / 1000) - 5) } });
  await coded("IOS_BUILD_UPLOAD_EXPIRED", urlExpired.run());
  assert.equal(urlExpired.fake.puts.length, 0);
  // expiry reached between two parts: the first part went out, the second is refused and nothing is committed
  let calls = 0;
  const midway = setup({ partSize: 5, upload: { expiration: "2026-10-18T10:05:00Z" } }, { now: () => NOW + (calls++ < 3 ? 0 : 10 * 60_000) });
  await coded("IOS_BUILD_UPLOAD_EXPIRED", midway.run());
  assert.equal(midway.fake.puts.length, 1);
  assert.ok(!api(midway.fake).includes("PATCH /v1/buildUploadFiles/bf1"));
});

test("credential headers are refused (any case) before a byte is sent", async () => {
  for (const name of ["Authorization", "authorization", "COOKIE", "Proxy-Authorization"]) {
    const t = setup({ upload: { headers: [{ name: "Content-Type", value: "x" }, { name, value: "Bearer abc" }] } });
    await coded("IOS_BUILD_UPLOAD_INVALID", t.run());
    assert.equal(t.fake.puts.length, 0, name);
  }
  const crlf = setup({ upload: { headers: [{ name: "X-Test", value: "a\r\nb" }] } });
  await coded("IOS_BUILD_UPLOAD_INVALID", crlf.run());
});

test("only Apple's upload hosts are accepted: look-alike hosts, http, URL credentials and non-PUT methods are refused", async () => {
  const op = (url, extra = {}) => ({ method: "PUT", url, offset: 0, length: 10, requestHeaders: [], ...extra });
  const ok = (url) => assert.doesNotThrow(() => assertUploadOperation(op(url), { now: NOW }), url);
  const bad = (url, extra) => assert.throws(() => assertUploadOperation(op(url, extra), { now: NOW }), (error) => error.code === "IOS_BUILD_UPLOAD_INVALID", url);
  assert.deepEqual([...UPLOAD_HOST_SUFFIXES], ["blobstore.apple.com"]);
  ok("https://store-030.blobstore.apple.com/p?Expires=9999999999");
  ok("https://blobstore.apple.com/p");
  for (const url of ["https://evil.example.com/p", "https://blobstore.apple.com.evil.com/p", "https://evilblobstore.apple.com/p", "https://apple.com/p", "https://store.apple.com/p", "http://store-030.blobstore.apple.com/p", "https://user:pw@store-030.blobstore.apple.com/p", "not a url"]) bad(url);
  bad("https://store-030.blobstore.apple.com/p", { method: "POST" });
  const host = setup({ upload: { opHost: "evil.example.com" } });
  await assert.rejects(host.run(), (error) => error.code === "IOS_BUILD_UPLOAD_INVALID" && /evil\.example\.com/.test(error.message) && !error.message.includes("secret-signature"));
  assert.equal(host.fake.puts.length, 0);
});

test("a gap, an overlap and an empty operation list are refused before any byte is sent", async () => {
  const run = (operations) => {
    const t = setup({}, {});
    const existing = { id: "bu9", state: "AWAITING_UPLOAD", files: [{ id: "bf9", fileName: "A.ipa", fileSize: 10, operations }] };
    const again = setup({ upload: { existing: [existing] } }, { entries: [ours("bu9")] });
    return again.run().then(() => again, (error) => { error.fake = again.fake; throw error; });
  };
  const op = (offset, length) => ({ method: "PUT", url: "https://store-030.blobstore.apple.com/p", offset, length, requestHeaders: [] });
  for (const operations of [[op(0, 4), op(5, 5)], [op(0, 6), op(4, 6)], [op(0, 5)], []]) await assert.rejects(run(operations), (error) => error.code === "IOS_BUILD_UPLOAD_INVALID" && error.fake.puts.length === 0);
});

test("bad inputs are refused and the dry-run plan carries no secret", async () => {
  await coded("IOS_BUILD_UPLOAD_CONFIG", setup({}, { options: { appId: "app" } }).run());
  await coded("IOS_BUILD_UPLOAD_CONFIG", setup({}, { source: { size: 0, sha256: "x", read() {} } }).run());
  const plan = buildUploadPlan({ appId: "1", marketingVersion: "1.0", buildNumber: "7", ipa: { name: "A.ipa", sizeBytes: 10, sha256: "a".repeat(64) } });
  assert.deepEqual(plan.map((step) => step.id), ["buildUploads.find", "buildUploads.create", "buildUploadFiles.reserve", "uploadOperations.put", "buildUploadFiles.commit", "buildUploads.poll"]);
  assert.ok(!JSON.stringify(plan).match(/PRIVATE|Bearer/));
});

// ---- N1: only our own journalled record is ever adopted --------------------------------------------------------------------
test("a COMPLETE record for this version and build that is not in our journal is another binary: refused, nothing sent, nothing adopted", async () => {
  const t = setup({ upload: { existing: [{ id: "bu77", state: "COMPLETE" }] } });
  await assert.rejects(t.run(), (error) => error.code === "IOS_BUILD_UPLOAD_FOREIGN" && /bu77/.test(error.message) && /COMPLETE/.test(error.message) && /new build number/.test(error.message));
  assert.deepEqual(api(t.fake), ["GET /v1/apps/6806854037/buildUploads"], "not even read further, and no create");
  assert.equal(t.fake.puts.length, 0);
  const processing = setup({ upload: { existing: [{ id: "bu78", state: "PROCESSING" }] } });
  await coded("IOS_BUILD_UPLOAD_FOREIGN", processing.run());
});

test("a foreign AWAITING_UPLOAD record is never filled with our bytes, even beside a journal for other records", async () => {
  const t = setup({ upload: { existing: [{ id: "bu79", state: "AWAITING_UPLOAD", files: [{ id: "bf79", fileName: "A.ipa", fileSize: 10 }] }] } }, { entries: [ours("bu1-old", { ipaSha256: "1".repeat(64) })] });
  await assert.rejects(t.run(), (error) => error.code === "IOS_BUILD_UPLOAD_FOREIGN" && /bu79/.test(error.message));
  assert.equal(t.fake.puts.length, 0);
  assert.ok(!api(t.fake).some((call) => call.startsWith("PATCH") || call.startsWith("POST")));
  assert.deepEqual(t.journal.list.map((entry) => entry.buildUploadId), ["bu1-old"], "the journal is untouched");
});

test("our journalled record is resumed; a journal entry for different IPA bytes, another version or another file is refused", async () => {
  const resume = setup({ partSize: 5, upload: { existing: [{ id: "bu9", state: "AWAITING_UPLOAD", files: [{ id: "bf9", fileName: "A.ipa", fileSize: 10 }] }] } }, { entries: [ours("bu9", { buildUploadFileId: "bf9" })] });
  assert.equal((await resume.run()).resumed, true);
  assert.equal(resume.fake.puts.length, 2);
  const other = setup({ upload: { existing: [{ id: "bu9", state: "COMPLETE" }] } }, { entries: [ours("bu9", { ipaSha256: "2".repeat(64) })] });
  await assert.rejects(other.run(), (error) => error.code === "IOS_BUILD_UPLOAD_FOREIGN" && /different IPA/.test(error.message));
  assert.equal(other.fake.puts.length, 0);
  const version = setup({ upload: { existing: [{ id: "bu9", state: "COMPLETE" }] } }, { entries: [ours("bu9", { version: "0.0.1" })] });
  await coded("IOS_BUILD_UPLOAD_FOREIGN", version.run());
  const wrongFile = setup({ upload: { existing: [{ id: "bu9", state: "AWAITING_UPLOAD", files: [{ id: "bfX", fileName: "A.ipa", fileSize: 10 }] }] } }, { entries: [ours("bu9", { buildUploadFileId: "bf9" })] });
  await coded("IOS_BUILD_UPLOAD_FOREIGN", wrongFile.run());
  assert.equal(wrongFile.fake.puts.length, 0);
});

test("a checksum reported by the API that differs from our SHA-256 stops the adoption", async () => {
  const withChecksum = (checksum) => setup({ upload: { existing: [{ id: "bu9", state: "COMPLETE", files: [{ id: "bf9", fileName: "A.ipa", fileSize: 10, checksum }] }] } }, { entries: [ours("bu9")] });
  await coded("IOS_BUILD_UPLOAD_FOREIGN", withChecksum({ hash: "3".repeat(64), algorithm: "SHA_256" }).run());
  assert.equal((await withChecksum({ hash: IDENTITY.ipaSha256.toUpperCase(), algorithm: "SHA_256" }).run()).resumed, true);
  assert.equal((await withChecksum({ hash: "x", algorithm: "MD5" }).run()).resumed, true);
});

test("a journal is required", async () => {
  const t = setup({}, {});
  await coded("IOS_BUILD_UPLOAD_CONFIG", uploadIpaToAppStoreConnect({ client: createAscClient({ tokenProvider: { get: () => "j" }, fetchImpl: t.fake.fetchImpl }), appId: "6806854037", marketingVersion: "1", buildNumber: "2", source: t.source, fileName: "A.ipa" }));
});
