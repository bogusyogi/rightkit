import assert from "node:assert/strict";
import { createHash, generateKeyPairSync, verify, X509Certificate } from "node:crypto";
import { mkdtempSync, mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { spawnSync } from "./background-process.mjs";
import { appleProfileAuth, configuredAppleProfile, decodeAppleProfile, describeAppleProfile, entitlementsSuperset, main, renewAppleProfile, signAppleApiJwt } from "./apple-profile-renew.mjs";

// Synthetic public certificate fixtures; no Apple private signing material.
const releaseCertificate = "MIIDizCCAnOgAwIBAgIUOVtNhnSONprpVGyOy5KS4rF7WawwDQYJKoZIhvcNAQELBQAwVTE+MDwGA1UEAww1RGV2ZWxvcGVyIElEIEFwcGxpY2F0aW9uOiBBZHJpYW4gRCdzb3V6YSAoNktMR0QzTExLRikxEzARBgNVBAsMCjZLTEdEM0xMS0YwHhcNMjYxMDA4MDY0NzExWhcNMzYxMDA1MDY0NzExWjBVMT4wPAYDVQQDDDVEZXZlbG9wZXIgSUQgQXBwbGljYXRpb246IEFkcmlhbiBEJ3NvdXphICg2S0xHRDNMTEtGKTETMBEGA1UECwwKNktMR0QzTExLRjCCASIwDQYJKoZIhvcNAQEBBQADggEPADCCAQoCggEBAJhlP4N8VQKkkHb7DA6vupCpMfdcN7cSXxo58PxZs0aVx+wGm+UIcTWoVzOtc+A2j1Jt+XCu2kum+fzeem9N0qWbxbLpMZD/2nh2EixN7cKtHIi+BM+1K1RGLWNQ7SHov36k6xUFueeWAkgs/h415yB/Wvl3m9htBjy2VnNpykEN1xoZ67WofhC39gxgP016xxFHHGHuieefPCkZ32xlc8dIfeWKlaiIzmJ1QSTIVQob94NKnehn+17S6uuSNxxt3sr+5Pp9vmHAqE3rzbVeLmwo1HP3jaduXpZxE1WgAlS0i9P4ZIrwvqU+zyQ8o1Ry9Sukq7KhFJPEivZIyfQLO7sCAwEAAaNTMFEwHQYDVR0OBBYEFD+GLnlvn6meULbXJax7aj4cQXUcMB8GA1UdIwQYMBaAFD+GLnlvn6meULbXJax7aj4cQXUcMA8GA1UdEwEB/wQFMAMBAf8wDQYJKoZIhvcNAQELBQADggEBAEvWu0nBG5Y3GGqSBp3JAjfcDsZ6xtaDbidWHuBWjrCMOGBSu4uGf0ytWGRJo3G0BGBi8vnr1TwTOQ6Pe0deEoMyqycMqGYxBNF8xhblBJ2aWLRyJPe64NNlwbvKZTBrnJuqOVVKLAwxixgbFE6LYj+QODBxIy2z7dAPBcpLuIiBHcVxncKIteoZRyxOqvq47z8QdEFUqo4XruqkkZ97BajLwkpfoMnM57gXQglvNWPOFsmXS26PV1K0LEsCu90VXyRs9SVaSrWkBvuTo1n55+/OLyLNYPTLJhkF8a9XyXQWTBcL+qpWDDv+4Js+PoNTyv2IDu8oiDQ/JvXTAv6lhHc=";
const developmentCertificate = "MIIDfTCCAmWgAwIBAgIUSdJGaBUVphuaRHDPPHdfv6wuYqcwDQYJKoZIhvcNAQELBQAwTjE3MDUGA1UEAwwuQXBwbGUgRGV2ZWxvcG1lbnQ6IEFkcmlhbiBEJ3NvdXphICg4NzU3S0w5ODlSKTETMBEGA1UECwwKODc1N0tMOTg5UjAeFw0yNjEwMDgwNjQ3MTFaFw0zNjEwMDUwNjQ3MTFaME4xNzA1BgNVBAMMLkFwcGxlIERldmVsb3BtZW50OiBBZHJpYW4gRCdzb3V6YSAoODc1N0tMOTg5UikxEzARBgNVBAsMCjg3NTdLTDk4OVIwggEiMA0GCSqGSIb3DQEBAQUAA4IBDwAwggEKAoIBAQDPkONHjVz4oJf4U8YeYUrL9wXVmUUOeh5yrsVsXxqWypQw1l0Fl9/fRHpTJ7DNxUc56Vpm39LNnRXeCJcOEN7V7baUedWNQYSDXjyDEvoHX1g4BUmswRGblNvjS17qLItO8H3LfovGFSlVTfsjgp/liH0vHx1msRxLUKpdsqFy29phk33RqBQJm28aaLglfaiTCkYKT9FcTmq0x+bhOK7hFIp059hqaIT06VKcXJ4Abmwiu7krL2Ni2m812NdPz4bSJim6890V9bXCnpH4A/1Goxp2a3KmpGtp/USZhtqgo+Vi2i6kVEmazOVtVWi3p1ee4iPYEWwGxhLaiH0pg2vfAgMBAAGjUzBRMB0GA1UdDgQWBBTg8C9vvsB3ZTVzuvZVJHPhMwA4lTAfBgNVHSMEGDAWgBTg8C9vvsB3ZTVzuvZVJHPhMwA4lTAPBgNVHRMBAf8EBTADAQH/MA0GCSqGSIb3DQEBCwUAA4IBAQDAxnLum9vCtTt939b8blQkec/qr3yv7yHkWIeCOnZU3cjKuzwOtavKeCrG3fmIIDYJlr6scAq77ua+Z6NDrQ13hrm99rUSJgQn6A0TXfbazKFcI5SlO11ZnHTN1XYjV9hxO3BMZSueUs3GBA+tofZ6s/rjk2WTTBUFNqmEUkhkF8f/YoWleIZFPdB6fofSEdqmLb5CBp5FVgWnb+MqkTtk3BkSx2KGNXOQUIGMCdch0gwuzA4fjDR572TrZc2/DBX9yO1W2im5RoLt6zmf/ZLguQQHBQLCm4TEcrHxou8DbSTPTb/hIuyDYi1uCoIEoxsiZ561QacFRQMMMi2CGfaA";
const now = new Date("2026-10-09T12:00:00Z");
const digest = (data) => createHash("sha1").update(Buffer.from(data, "base64")).digest("hex").toUpperCase();
const leaf = new X509Certificate(Buffer.from(releaseCertificate, "base64"));
const certSha1 = digest(releaseCertificate);
const profile = () => ({
  Name: "HeardRightDesktopCloudKitPush-20260928", TeamIdentifier: ["6KLGD3LLKF"], ApplicationIdentifierPrefix: ["6KLGD3LLKF"],
  Platform: ["OSX"], ProvisionsAllDevices: true, ExpirationDate: "2030-01-01T00:00:00Z", DeveloperCertificates: [releaseCertificate],
  Entitlements: { "com.apple.application-identifier": "6KLGD3LLKF.app.heardright.next", "com.apple.developer.team-identifier": "6KLGD3LLKF", "com.apple.developer.icloud-services": ["CloudKit"], "com.apple.developer.icloud-container-identifiers": ["iCloud.app.heardright"], "com.apple.security.get-task-allow": false, "aps-environment": "production" },
});
function fixture(t, overrides = {}) {
  const root = mkdtempSync(path.join(os.tmpdir(), "rightkit-apple-profile-test-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const file = path.join(root, "current.provisionprofile");
  const keyPath = path.join(root, "AuthKey_SYNTHETIC.p8");
  writeFileSync(file, "original CMS bytes");
  writeFileSync(keyPath, "synthetic sentinel; JWT signer is mocked");
  const previous = profile(), next = profile();
  next.Name = "HeardRightDesktopCloudKitPush-20261009";
  next.Entitlements["com.apple.developer.icloud-services"].push("CloudDocuments");
  next.Entitlements["extra-capability"] = true;
  const calls = [], localCalls = [], logs = [], signerCalls = [];
  let currentFile;
  const run = (command, args, options) => {
    localCalls.push({ command, args, options });
    if (command === "security" && args[0] === "cms") { currentFile = args.at(-1); return { status: 0, stdout: "mock plist" }; }
    if (command === "python3") return { status: 0, stdout: JSON.stringify(currentFile === file ? previous : next) };
    if (command === "security" && args[0] === "find-identity") return { status: 0, stdout: `1) ${certSha1} "Developer ID Application: Adrian D'souza (6KLGD3LLKF)"` };
    if (command === "security" && args[0] === "find-certificate") return { status: 0, stdout: leaf.toString() };
    throw new Error(`Unexpected local command ${command}`);
  };
  const fetchApi = async (url, options) => {
    calls.push({ url: new URL(url), options });
    const endpoint = new URL(url).pathname;
    let data;
    if (endpoint === "/v1/certificates") data = [{ type: "certificates", id: "CERT-NEW", attributes: { serialNumber: leaf.serialNumber } }];
    else if (endpoint === "/v1/bundleIds") data = [{ type: "bundleIds", id: "BUNDLE-1", attributes: { identifier: "app.heardright.next", platform: "MAC_OS" } }];
    else if (endpoint === "/v1/profiles") data = { type: "profiles", id: "PROFILE-NEW", attributes: { profileContent: Buffer.from("renewed CMS bytes").toString("base64") } };
    else throw new Error(`Unexpected API endpoint ${endpoint}`);
    return { ok: true, status: options.method === "POST" ? 201 : 200, json: async () => ({ data }) };
  };
  const options = { root, config: { targets: { mac: { sign: { provisioningProfile: "current.provisionprofile" } } } }, env: { APPLE_API_ISSUER: "synthetic-issuer", APPLE_API_KEY: "SYNTHETIC", APPLE_API_KEY_PATH: keyPath }, run, fetch: fetchApi, jwtSigner: (auth) => { signerCalls.push(auth); return "synthetic-jwt"; }, now, log: (value) => logs.push(value), ...overrides };
  return { root, file, keyPath, previous, next, calls, localCalls, logs, signerCalls, options, fetchApi, run };
}

test("renewal discovers by serial, creates & verifies profile, preserves adjacent backup", async (t) => {
  const f = fixture(t);
  const result = await renewAppleProfile(f.options);
  assert.equal(result.certificate.sha1, certSha1);
  assert.equal(result.certificate.id, "CERT-NEW");
  assert.equal(result.bundleId.id, "BUNDLE-1");
  assert.equal(f.calls[0].url.searchParams.get("filter[serialNumber]"), leaf.serialNumber);
  assert.equal(f.calls[1].url.searchParams.get("filter[identifier]"), "app.heardright.next");
  assert.deepEqual(f.calls.map((call) => call.options.method), ["GET", "GET", "POST"]);
  assert.deepEqual(JSON.parse(f.calls[2].options.body), { data: { type: "profiles", attributes: { name: "HeardRightDesktopCloudKitPush-20261009", profileType: "MAC_APP_DIRECT" }, relationships: { bundleId: { data: { type: "bundleIds", id: "BUNDLE-1" } }, certificates: { data: [{ type: "certificates", id: "CERT-NEW" }] } } } });
  assert.equal(readFileSync(f.file, "utf8"), "renewed CMS bytes");
  assert.equal(readFileSync(result.backupPath, "utf8"), "original CMS bytes");
  assert.equal(path.dirname(result.backupPath), path.dirname(f.file));
  assert.ok(f.localCalls.some((call) => call.command === "security" && call.args[0] === "cms"));
  assert.deepEqual(f.signerCalls, [{ keyPath: f.keyPath, keyId: "SYNTHETIC", issuer: "synthetic-issuer" }]);
  assert.doesNotMatch(f.logs.join("\n"), /synthetic-jwt|synthetic sentinel/);
});

test("dry-run prints choices & planned POST, performs GETs only, writes nothing", async (t) => {
  const f = fixture(t);
  const before = readdirSync(f.root);
  const result = await renewAppleProfile({ ...f.options, dryRun: true });
  assert.equal(result.dryRun, true);
  assert.deepEqual(f.calls.map((call) => call.options.method), ["GET", "GET"]);
  assert.deepEqual(readdirSync(f.root), before);
  assert.equal(readFileSync(f.file, "utf8"), "original CMS bytes");
  assert.match(f.logs.join("\n"), /PLANNED POST \/v1\/profiles/);
  assert.match(f.logs.join("\n"), new RegExp(certSha1));
  assert.match(f.logs.join("\n"), /BUNDLE-1/);
});

test("no matching private-key identity fails before authentication or fetch", async (t) => {
  const f = fixture(t);
  const run = (command, args, opts) => command === "security" && args[0] === "find-identity" ? { status: 0, stdout: "0 valid identities found" } : f.run(command, args, opts);
  await assert.rejects(renewAppleProfile({ ...f.options, run }), { code: "APPLE_PROFILE_SIGNING_CERT_MISSING" });
  assert.equal(f.calls.length, 0);
  assert.equal(f.signerCalls.length, 0);
});

for (const forbiddenMethod of ["GET", "POST"]) test(`403 on ${forbiddenMethod} is typed & preserves current file`, async (t) => {
  const f = fixture(t);
  const fetchApi = (url, options) => options.method === forbiddenMethod ? Promise.resolve({ ok: false, status: 403 }) : f.fetchApi(url, options);
  await assert.rejects(renewAppleProfile({ ...f.options, fetch: fetchApi }), { code: "APPLE_PROFILE_INSUFFICIENT_ROLE" });
  assert.equal(readFileSync(f.file, "utf8"), "original CMS bytes");
  assert.ok(!readdirSync(f.root).some((file) => file.includes("backup")));
});

for (const mutate of [
  (next) => { delete next.Entitlements["aps-environment"]; },
  (next) => { next.Entitlements["com.apple.developer.icloud-services"] = ["CloudDocuments"]; },
  (next) => { next.Entitlements["aps-environment"] = "development"; },
]) test("entitlement regression rejects replacement & preserves old profile", async (t) => {
  const f = fixture(t); mutate(f.next);
  await assert.rejects(renewAppleProfile(f.options), { code: "APPLE_PROFILE_ENTITLEMENTS_MISMATCH" });
  assert.equal(readFileSync(f.file, "utf8"), "original CMS bytes");
  assert.deepEqual(readdirSync(f.root).sort(), ["AuthKey_SYNTHETIC.p8", "current.provisionprofile"]);
});

test("superset comparison preserves nested values, permits added array members & keys", () => {
  assert.equal(entitlementsSuperset({ a: ["one", "two"], b: { enabled: true, extra: 1 } }, { a: ["one"], b: { enabled: true } }), true);
  assert.equal(entitlementsSuperset({ a: ["two"] }, { a: ["one"] }), false);
  assert.equal(entitlementsSuperset({ a: true }, { a: false }), false);
  assert.equal(entitlementsSuperset({}, { a: undefined }), false);
});

for (const [field, expected] of [["TeamIdentifier", "APPLE_PROFILE_TEAM_MISMATCH"], ["DeveloperCertificates", "APPLE_PROFILE_CERT_MISMATCH"], ["ExpirationDate", "APPLE_PROFILE_INVALID"]]) test(`verification rejects invalid ${field}`, async (t) => {
  const f = fixture(t);
  f.next[field] = field === "TeamIdentifier" ? ["OTHERTEAM"] : field === "DeveloperCertificates" ? [developmentCertificate] : "2020-01-01";
  await assert.rejects(renewAppleProfile(f.options), { code: expected });
  assert.equal(readFileSync(f.file, "utf8"), "original CMS bytes");
});

for (const [endpoint, code] of [["certificates", "APPLE_PROFILE_CERT_NOT_IN_ASC"], ["bundleIds", "APPLE_PROFILE_BUNDLE_ID_MISSING"]]) test(`missing ${endpoint} gives typed error without POST`, async (t) => {
  const f = fixture(t);
  const fetchApi = (url, options) => new URL(url).pathname === `/v1/${endpoint}` ? Promise.resolve({ ok: true, status: 200, json: async () => ({ data: [] }) }) : f.fetchApi(url, options);
  await assert.rejects(renewAppleProfile({ ...f.options, fetch: fetchApi }), { code });
  assert.ok(f.calls.every((call) => call.options.method === "GET"));
});

test("missing issuer, key id or file gives typed auth errors", (t) => {
  const f = fixture(t);
  assert.throws(() => appleProfileAuth({}), { code: "APPLE_PROFILE_ISSUER_MISSING" });
  assert.throws(() => appleProfileAuth({ APPLE_API_ISSUER: "issuer" }), { code: "APPLE_PROFILE_KEY_MISSING" });
  assert.throws(() => appleProfileAuth({ ...f.options.env, APPLE_API_KEY_PATH: path.join(f.root, "absent.p8") }), { code: "APPLE_PROFILE_KEY_MISSING" });
});

test("JWT signer uses in-process ES256 with issuer, key id & bounded expiry", (t) => {
  const f = fixture(t);
  const { privateKey, publicKey } = generateKeyPairSync("ec", { namedCurve: "prime256v1" });
  writeFileSync(f.keyPath, privateKey.export({ type: "pkcs8", format: "pem" }));
  const token = signAppleApiJwt({ keyPath: f.keyPath, keyId: "SYNTHETIC", issuer: "synthetic-issuer" }, now);
  const [header, payload, signature] = token.split(".");
  assert.deepEqual(JSON.parse(Buffer.from(header, "base64url")), { alg: "ES256", kid: "SYNTHETIC", typ: "JWT" });
  const claims = JSON.parse(Buffer.from(payload, "base64url"));
  assert.equal(claims.iss, "synthetic-issuer");
  assert.equal(claims.aud, "appstoreconnect-v1");
  assert.equal(claims.exp - claims.iat, 600);
  assert.equal(verify("sha256", Buffer.from(`${header}.${payload}`), { key: publicKey, dsaEncoding: "ieee-p1363" }, Buffer.from(signature, "base64url")), true);
});

test("invalid key error never exposes key contents", (t) => {
  const f = fixture(t);
  assert.throws(() => signAppleApiJwt({ keyPath: f.keyPath }, now), (error) => error.code === "APPLE_PROFILE_KEY_INVALID" && !error.message.includes("synthetic sentinel"));
});

test("profile resolution supports release config, Tauri embedding & explicit override", (t) => {
  const f = fixture(t);
  mkdirSync(path.join(f.root, "native"));
  writeFileSync(path.join(f.root, "native/tauri.conf.json"), JSON.stringify({ bundle: { macOS: { files: { "embedded.provisionprofile": "profile.provisionprofile" } } } }));
  const config = { development: { targets: { mac: { install: { manifestPath: "native/Cargo.toml" } } } } };
  assert.equal(configuredAppleProfile({ root: f.root, config }), path.join(f.root, "native/profile.provisionprofile"));
  assert.equal(configuredAppleProfile({ root: f.root, config: f.options.config }), f.file);
  assert.equal(configuredAppleProfile({ root: f.root, config, profilePath: "override.provisionprofile" }), path.join(f.root, "override.provisionprofile"));
});

test("development renewal selects Apple Development certificate & retains registered devices", async (t) => {
  const f = fixture(t);
  for (const p of [f.previous, f.next]) {
    p.ProvisionsAllDevices = false; p.ProvisionedDevices = ["MAC-UDID"];
    p.TeamIdentifier = ["8757KL989R"]; p.ApplicationIdentifierPrefix = ["8757KL989R"];
    p.DeveloperCertificates = [developmentCertificate];
    p.Entitlements["com.apple.application-identifier"] = "8757KL989R.app.heardright.next";
    p.Entitlements["com.apple.developer.team-identifier"] = "8757KL989R";
    p.Entitlements["com.apple.security.get-task-allow"] = true;
  }
  const developmentLeaf = new X509Certificate(Buffer.from(developmentCertificate, "base64"));
  const run = (command, args, opts) => {
    if (command === "security" && args[0] === "find-identity") return { status: 0, stdout: `1) ${digest(developmentCertificate)} "Apple Development: Adrian D'souza (8757KL989R)"` };
    if (command === "security" && args[0] === "find-certificate") return { status: 0, stdout: developmentLeaf.toString() };
    return f.run(command, args, opts);
  };
  const fetchApi = (url, options) => {
    const endpoint = new URL(url).pathname;
    if (endpoint === "/v1/certificates") return Promise.resolve({ ok: true, status: 200, json: async () => ({ data: [{ id: "DEV-CERT", attributes: { serialNumber: developmentLeaf.serialNumber } }] }) });
    if (endpoint === "/v1/devices") return Promise.resolve({ ok: true, status: 200, json: async () => ({ data: [{ id: "DEVICE-1", attributes: { udid: "MAC-UDID", status: "ENABLED" } }] }) });
    return f.fetchApi(url, options);
  };
  const result = await renewAppleProfile({ ...f.options, run, fetch: fetchApi });
  assert.equal(result.profileType, "MAC_APP_DEVELOPMENT");
  assert.equal(result.certificate.sha1, digest(developmentCertificate));
  assert.deepEqual(result.body.data.relationships.devices, { data: [{ type: "devices", id: "DEVICE-1" }] });
});

test("profileContent fallback GET downloads new profile", async (t) => {
  const f = fixture(t);
  const fetchApi = (url, options) => {
    if (options.method === "POST") return Promise.resolve({ ok: true, status: 201, json: async () => ({ data: { id: "PROFILE-NEW" } }) });
    if (new URL(url).pathname === "/v1/profiles/PROFILE-NEW") return Promise.resolve({ ok: true, status: 200, json: async () => ({ data: { id: "PROFILE-NEW", attributes: { profileContent: Buffer.from("downloaded CMS").toString("base64") } } }) });
    return f.fetchApi(url, options);
  };
  await renewAppleProfile({ ...f.options, fetch: fetchApi });
  assert.equal(readFileSync(f.file, "utf8"), "downloaded CMS");
});

test("API pagination follows next links & finds certificate on later page", async (t) => {
  const f = fixture(t);
  const fetchApi = (url, options) => new URL(url).pathname === "/v1/certificates" && !new URL(url).searchParams.has("cursor")
    ? Promise.resolve({ ok: true, status: 200, json: async () => ({ data: [], links: { next: `${url}&cursor=next` } }) }) : f.fetchApi(url, options);
  assert.equal((await renewAppleProfile({ ...f.options, fetch: fetchApi, dryRun: true })).certificate.id, "CERT-NEW");
});

test("concurrent profile change cannot be overwritten", async (t) => {
  const f = fixture(t);
  const run = (command, args, options) => {
    if (command === "security" && args[0] === "cms" && args.at(-1) !== f.file) writeFileSync(f.file, "other writer's profile");
    return f.run(command, args, options);
  };
  await assert.rejects(renewAppleProfile({ ...f.options, run }), { code: "APPLE_PROFILE_CHANGED" });
  assert.equal(readFileSync(f.file, "utf8"), "other writer's profile");
});

test("CLI parser runs mocked dry-run from config & rejects malformed options", async (t) => {
  const f = fixture(t);
  const configFile = path.join(f.root, "right-release.config.mjs");
  writeFileSync(configFile, `export default ${JSON.stringify(f.options.config)}`);
  const result = await main(["profile", "renew", "--config", configFile, "--dry-run"], f.options);
  assert.equal(result.dryRun, true);
  await assert.rejects(main(["profile", "renew", "--profile"]), { code: "APPLE_PROFILE_USAGE" });
  const cli = spawnSync(process.execPath, [new URL("./cli/right-release.mjs", import.meta.url).pathname, "apple", "profile", "renew", "--help"], { encoding: "utf8" });
  assert.equal(cli.status, 0, cli.stderr);
  assert.match(cli.stdout, /APPLE_API_ISSUER/);
  assert.match(cli.stdout, /GET discovery only/);
});

test("CMS decoder handles real plist data & dates through parser without Apple calls", (t) => {
  const f = fixture(t);
  const xml = `<?xml version="1.0"?><plist version="1.0"><dict><key>Name</key><string>Fixture</string><key>DeveloperCertificates</key><array><data>${releaseCertificate}</data></array><key>ExpirationDate</key><date>2030-01-01T00:00:00Z</date></dict></plist>`;
  const run = (command, args, options) => command === "security" ? { status: 0, stdout: xml } : spawnSync(command, args, options);
  const result = decodeAppleProfile(f.file, { run });
  assert.equal(result.DeveloperCertificates[0], releaseCertificate);
  assert.equal(result.ExpirationDate, "2030-01-01T00:00:00");
});

test("unsupported profile type fails closed", () => {
  const p = profile(); delete p.ProvisionsAllDevices;
  assert.throws(() => describeAppleProfile(p), { code: "APPLE_PROFILE_TYPE_UNSUPPORTED" });
});

test("stale profile certificate does not constrain current Keychain signer", async (t) => {
  const f = fixture(t);
  f.previous.DeveloperCertificates = [developmentCertificate];
  const result = await renewAppleProfile(f.options);
  assert.equal(result.certificate.sha1, certSha1);
  assert.equal(result.body.data.relationships.certificates.data[0].id, "CERT-NEW");
});

test("repeated renewal keeps every prior adjacent backup", async (t) => {
  const f = fixture(t);
  const first = await renewAppleProfile(f.options);
  const second = await renewAppleProfile(f.options);
  assert.notEqual(first.backupPath, second.backupPath);
  assert.equal(readFileSync(first.backupPath, "utf8"), "original CMS bytes");
  assert.equal(readFileSync(second.backupPath, "utf8"), "renewed CMS bytes");
  assert.equal(f.calls.filter((call) => call.options.method === "POST").length, 2);
  assert.ok(f.calls.every((call) => ["GET", "POST"].includes(call.options.method)));
});
