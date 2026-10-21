import assert from "node:assert/strict";
import { createHash, generateKeyPairSync, verify, X509Certificate } from "node:crypto";
import { mkdtempSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { buildCsr, containerManualSteps, createAppleProfile, main, parseCreateArgs, verifyDirectProfile } from "./apple-profile-create.mjs";

// Mocked fetch and mocked local commands only: no network, no Keychain, no Apple call.
// Synthetic public certificate fixture (no private signing material).
const developmentCertificate = "MIIDfTCCAmWgAwIBAgIUSdJGaBUVphuaRHDPPHdfv6wuYqcwDQYJKoZIhvcNAQELBQAwTjE3MDUGA1UEAwwuQXBwbGUgRGV2ZWxvcG1lbnQ6IEFkcmlhbiBEJ3NvdXphICg4NzU3S0w5ODlSKTETMBEGA1UECwwKODc1N0tMOTg5UjAeFw0yNjEwMDgwNjQ3MTFaFw0zNjEwMDUwNjQ3MTFaME4xNzA1BgNVBAMMLkFwcGxlIERldmVsb3BtZW50OiBBZHJpYW4gRCdzb3V6YSAoODc1N0tMOTg5UikxEzARBgNVBAsMCjg3NTdLTDk4OVIwggEiMA0GCSqGSIb3DQEBAQUAA4IBDwAwggEKAoIBAQDPkONHjVz4oJf4U8YeYUrL9wXVmUUOeh5yrsVsXxqWypQw1l0Fl9/fRHpTJ7DNxUc56Vpm39LNnRXeCJcOEN7V7baUedWNQYSDXjyDEvoHX1g4BUmswRGblNvjS17qLItO8H3LfovGFSlVTfsjgp/liH0vHx1msRxLUKpdsqFy29phk33RqBQJm28aaLglfaiTCkYKT9FcTmq0x+bhOK7hFIp059hqaIT06VKcXJ4Abmwiu7krL2Ni2m812NdPz4bSJim6890V9bXCnpH4A/1Goxp2a3KmpGtp/USZhtqgo+Vi2i6kVEmazOVtVWi3p1ee4iPYEWwGxhLaiH0pg2vfAgMBAAGjUzBRMB0GA1UdDgQWBBTg8C9vvsB3ZTVzuvZVJHPhMwA4lTAfBgNVHSMEGDAWgBTg8C9vvsB3ZTVzuvZVJHPhMwA4lTAPBgNVHRMBAf8EBTADAQH/MA0GCSqGSIb3DQEBCwUAA4IBAQDAxnLum9vCtTt939b8blQkec/qr3yv7yHkWIeCOnZU3cjKuzwOtavKeCrG3fmIIDYJlr6scAq77ua+Z6NDrQ13hrm99rUSJgQn6A0TXfbazKFcI5SlO11ZnHTN1XYjV9hxO3BMZSueUs3GBA+tofZ6s/rjk2WTTBUFNqmEUkhkF8f/YoWleIZFPdB6fofSEdqmLb5CBp5FVgWnb+MqkTtk3BkSx2KGNXOQUIGMCdch0gwuzA4fjDR572TrZc2/DBX9yO1W2im5RoLt6zmf/ZLguQQHBQLCm4TEcrHxou8DbSTPTb/hIuyDYi1uCoIEoxsiZ561QacFRQMMMi2CGfaA";
const leaf = new X509Certificate(Buffer.from(developmentCertificate, "base64"));
const certSha1 = createHash("sha1").update(Buffer.from(developmentCertificate, "base64")).digest("hex").toUpperCase();
const now = new Date("2026-10-16T12:00:00Z");
const BUNDLE = "app.heardright.next", CONTAINER = "iCloud.app.heardright.ios.native", NAME = "HeardRight CloudKit Dev";
const UDID = "00008103-AAAA";

const issued = (overrides = {}) => ({
  Platform: ["OSX"], ExpirationDate: "2030-01-01T00:00:00Z", DeveloperCertificates: [developmentCertificate], ProvisionedDevices: [UDID],
  Entitlements: { "com.apple.application-identifier": `6KLGD3LLKF.${BUNDLE}`, "com.apple.security.get-task-allow": true, "com.apple.developer.icloud-services": ["CloudKit"], "com.apple.developer.icloud-container-identifiers": [CONTAINER] },
  ...overrides,
});

// In-memory fake of the ASC endpoints this command uses.
function fixture(t, { state: patch = {}, decoded = issued(), localCertificate = developmentCertificate } = {}) {
  const root = mkdtempSync(path.join(os.tmpdir(), "rightkit-apple-create-test-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const keyPath = path.join(root, "AuthKey_SYNTHETIC.p8");
  writeFileSync(keyPath, "synthetic sentinel; JWT signer is mocked");
  const state = { bundles: [], capabilities: [], devices: [{ id: "DEV-1", attributes: { udid: UDID, status: "ENABLED", platform: "MAC_OS" } }], certificates: [], profiles: [], ...patch };
  const calls = [], logs = [], local = [];
  const reply = (data, status = 200) => Promise.resolve({ ok: status < 300, status, json: async () => ({ data }) });
  const fetchApi = async (url, options) => {
    const u = new URL(url), method = options.method, body = options.body ? JSON.parse(options.body) : undefined;
    calls.push({ method, path: u.pathname, search: u.search, body });
    const p = u.pathname;
    if (method === "GET" && p === "/v1/bundleIds") return reply(state.bundles);
    if (method === "POST" && p === "/v1/bundleIds") { const b = { type: "bundleIds", id: "BUNDLE-NEW", attributes: body.data.attributes }; state.bundles.push(b); return reply(b, 201); }
    if (method === "GET" && /^\/v1\/bundleIds\/[^/]+\/bundleIdCapabilities$/.test(p)) return reply(state.capabilities);
    if (method === "POST" && p === "/v1/bundleIdCapabilities") { if (state.capabilityStatus) return reply(null, state.capabilityStatus); const c = { id: "CAP-NEW", attributes: { capabilityType: "ICLOUD", settings: body.data.attributes.settings } }; state.capabilities.push(c); return reply(c, 201); }
    if (method === "PATCH" && p.startsWith("/v1/bundleIdCapabilities/")) return reply({ id: "CAP-1" });
    if (method === "GET" && p === "/v1/devices") return reply(state.devices);
    if (method === "POST" && p === "/v1/devices") { const d = { id: "DEV-NEW", attributes: { udid: body.data.attributes.udid, status: "ENABLED" } }; state.devices.push(d); return reply(d, 201); }
    if (method === "GET" && p === "/v1/certificates") return reply(state.certificates);
    if (method === "GET" && p.startsWith("/v1/certificates/")) return reply(state.certificates.find((c) => c.id === p.split("/").at(-1)) ?? null);
    if (method === "GET" && p === "/v1/profiles") return reply(state.profiles);
    const rel = /^\/v1\/profiles\/([^/]+)\/(bundleId|certificates|devices)$/.exec(p);
    if (method === "GET" && rel) { const profile = state.profiles.find((x) => x.id === rel[1]); return reply(rel[2] === "bundleId" ? profile.rels.bundleId : profile.rels[rel[2]]); }
    if (method === "POST" && p === "/v1/profiles") { const profile = { id: "PROFILE-NEW", attributes: { name: body.data.attributes.name, profileContent: Buffer.from("issued CMS bytes").toString("base64") } }; state.profiles.push(profile); return reply(profile, 201); }
    if (method === "DELETE" && p.startsWith("/v1/profiles/")) { state.profiles = state.profiles.filter((x) => x.id !== p.split("/").at(-1)); return Promise.resolve({ ok: true, status: 204, json: async () => { throw new Error("no body"); } }); }
    throw new Error(`Unexpected ${method} ${p}`);
  };
  const run = (command, args) => {
    local.push([command, ...args]);
    if (command === "security" && args[0] === "cms") return { status: 0, stdout: "mock plist" };
    if (command === "python3") return { status: 0, stdout: JSON.stringify(decoded) };
    if (command === "security" && args[0] === "find-identity") return { status: 0, stdout: `1) ${createHash("sha1").update(Buffer.from(localCertificate, "base64")).digest("hex").toUpperCase()} "Fixture signing identity"` };
    if (command === "security" && args[0] === "find-certificate") return { status: 0, stdout: new X509Certificate(Buffer.from(localCertificate, "base64")).toString() };
    throw new Error(`Unexpected local command ${command}`);
  };
  const out = path.join(root, "provisioning-dev", "Dev.provisionprofile");
  const options = { root, config: {}, bundleId: BUNDLE, name: NAME, containers: [CONTAINER], certificate: "create", out: path.relative(root, out), env: { APPLE_API_ISSUER: "synthetic-issuer", APPLE_API_KEY: "SYNTHETIC", APPLE_API_KEY_PATH: keyPath }, run, fetch: fetchApi, jwtSigner: () => "synthetic-jwt", now, log: (v) => logs.push(v), platform: "darwin", home: root };
  return { root, out, keyPath, state, calls, logs, local, options };
}

const readyState = () => ({
  bundles: [{ id: "BUNDLE-1", attributes: { identifier: BUNDLE, platform: "MAC_OS" } }],
  capabilities: [{ id: "CAP-1", attributes: { capabilityType: "ICLOUD", settings: [{ key: "ICLOUD_VERSION", options: [{ key: "XCODE_6", enabled: true }] }] } }],
  certificates: [{ id: "CERT-1", attributes: { certificateType: "DEVELOPMENT", serialNumber: leaf.serialNumber, expirationDate: "2030-01-01T00:00:00Z", certificateContent: developmentCertificate } }],
});
const mutations = (calls) => calls.filter((c) => c.method !== "GET");

test("dry-run is the default: plans every missing piece with GETs only and writes nothing", async (t) => {
  const f = fixture(t, { state: { devices: [{ id: "DEV-1", attributes: { udid: UDID, status: "ENABLED" } }] } });
  const result = await createAppleProfile({ ...f.options, dryRun: true });
  assert.equal(result.dryRun, true);
  assert.equal(mutations(f.calls).length, 0);
  assert.deepEqual(result.planned.map((p) => `${p.method} ${p.path.split(" ")[0]}`).filter((v) => v.startsWith("POST")), ["POST /v1/bundleIds", "POST /v1/bundleIdCapabilities", "POST /v1/certificates", "POST /v1/profiles"]);
  assert.equal(result.bundleId.action, "create");
  assert.equal(result.capability.action, "create");
  assert.equal(result.certificate.mode, "create");
  assert.equal(result.containers[0].assignment, "not-verifiable-via-api");
  assert.deepEqual(readdirSync(f.root).sort(), ["AuthKey_SYNTHETIC.p8"]);
  assert.match(f.logs.join("\n"), /PLANNED POST \/v1\/profiles/);
  assert.doesNotMatch(f.logs.join("\n"), /synthetic-jwt|synthetic sentinel|PRIVATE KEY/);
});

test("argument parser: dry-run is the default, --apply is explicit, both together are refused", () => {
  assert.equal(parseCreateArgs(["--bundle-id", BUNDLE, "--name", NAME, "--icloud-container", CONTAINER, "--out", "x"]).dryRun, true);
  assert.equal(parseCreateArgs(["--bundle-id", BUNDLE, "--name", NAME, "--icloud-container", CONTAINER, "--out", "x", "--apply"]).dryRun, false);
  assert.throws(() => parseCreateArgs(["--bundle-id", BUNDLE, "--name", NAME, "--icloud-container", CONTAINER, "--out", "x", "--apply", "--dry-run"]), { code: "APPLE_PROFILE_USAGE" });
});

test("idempotent reuse: existing bundle, capability, certificate and active profile cause no mutation", async (t) => {
  const state = readyState();
  state.profiles = [{ id: "PROFILE-1", attributes: { name: NAME, profileState: "ACTIVE", expirationDate: "2030-01-01T00:00:00Z", profileContent: Buffer.from("issued CMS bytes").toString("base64") },
    rels: { bundleId: { id: "BUNDLE-1" }, certificates: [{ id: "CERT-1" }], devices: [{ id: "DEV-1" }] } }];
  const f = fixture(t, { state });
  const result = await createAppleProfile({ ...f.options, dryRun: false });
  assert.equal(mutations(f.calls).length, 0);
  assert.equal(result.bundleId.action, "reuse");
  assert.equal(result.capability.action, "reuse");
  assert.equal(result.certificate.mode, "reuse");
  assert.equal(result.certificate.id, "CERT-1");
  assert.equal(result.profile.action, "reuse");
  assert.equal(readFileSync(f.out, "utf8"), "issued CMS bytes");
  if (process.platform !== "win32") assert.equal(statSync(f.out).mode & 0o777, 0o600);
  assert.match(f.logs.join("\n"), new RegExp(certSha1));
});

test("apply creates the profile with the certificate and every Mac device, verifies it, writes 0600", async (t) => {
  const f = fixture(t, { state: readyState() });
  const result = await createAppleProfile({ ...f.options, dryRun: false });
  const post = f.calls.find((c) => c.method === "POST" && c.path === "/v1/profiles");
  assert.deepEqual(post.body, { data: { type: "profiles", attributes: { name: NAME, profileType: "MAC_APP_DEVELOPMENT" }, relationships: {
    bundleId: { data: { type: "bundleIds", id: "BUNDLE-1" } }, certificates: { data: [{ type: "certificates", id: "CERT-1" }] }, devices: { data: [{ type: "devices", id: "DEV-1" }] } } } });
  assert.deepEqual(mutations(f.calls).map((c) => `${c.method} ${c.path}`), ["POST /v1/profiles"]);
  assert.equal(readFileSync(f.out, "utf8"), "issued CMS bytes");
  if (process.platform !== "win32") assert.equal(statSync(f.out).mode & 0o777, 0o600);
  assert.equal(result.identitySha1, certSha1);
  assert.ok(f.calls.some((c) => c.path === "/v1/certificates" && c.search.includes("filter%5BcertificateType%5D=DEVELOPMENT")));
});

test("a missing ICLOUD capability is created, and a refused creation fails with the manual steps", async (t) => {
  const f = fixture(t, { state: { ...readyState(), capabilities: [] } });
  const ok = await createAppleProfile({ ...f.options, dryRun: false });
  assert.equal(ok.capability.action, "create");
  const create = f.calls.find((c) => c.path === "/v1/bundleIdCapabilities");
  assert.equal(create.body.data.attributes.capabilityType, "ICLOUD");
  assert.deepEqual(create.body.data.attributes.settings, [{ key: "ICLOUD_VERSION", options: [{ key: "XCODE_6" }] }]);

  const g = fixture(t, { state: { ...readyState(), capabilities: [], capabilityStatus: 409 } });
  await assert.rejects(createAppleProfile({ ...g.options, dryRun: false }), (error) => error.code === "APPLE_PROFILE_CAPABILITY_MISSING"
    && /no endpoint to create or assign iCloud containers/.test(error.message) && error.message.includes(CONTAINER) && error.message.includes("Include CloudKit support"));
  assert.ok(!g.calls.some((c) => c.path === "/v1/profiles" && c.method === "POST"));
});

test("a container the profile lacks deletes the new profile, writes nothing and prints the manual steps", async (t) => {
  const f = fixture(t, { state: readyState(), decoded: issued({ Entitlements: { ...issued().Entitlements, "com.apple.developer.icloud-container-identifiers": ["iCloud.other"] } }) });
  await assert.rejects(createAppleProfile({ ...f.options, dryRun: false }), (error) => error.code === "APPLE_PROFILE_ICLOUD_CONTAINER_UNASSIGNED"
    && error.message.includes(CONTAINER) && error.message.includes("developer.apple.com"));
  assert.ok(f.calls.some((c) => c.method === "DELETE" && c.path === "/v1/profiles/PROFILE-NEW"));
  assert.deepEqual(readdirSync(path.dirname(f.out)), []);
});

test("inline PEM is refused in the environment, the config and --certificate, on every host", async (t) => {
  const f = fixture(t, { state: readyState() });
  const PEM = "-----BEGIN " + "PRIVATE KEY-----\nfake\n-----END PRIVATE KEY-----";
  for (const name of ["APPLE_API_KEY_PATH", "APPLE_API_KEY", "APPLE_API_ISSUER"]) {
    await assert.rejects(createAppleProfile({ ...f.options, env: { ...f.options.env, [name]: PEM } }), { code: "APPLE_PROFILE_INLINE_PEM_REFUSED" }, name);
  }
  await assert.rejects(createAppleProfile({ ...f.options, config: { appStoreConnect: { keyPath: PEM } } }), { code: "APPLE_PROFILE_INLINE_PEM_REFUSED" });
  assert.throws(() => parseCreateArgs(["--bundle-id", BUNDLE, "--name", NAME, "--icloud-container", CONTAINER, "--out", "x", "--certificate", PEM]), { code: "APPLE_PROFILE_USAGE" });
  assert.equal(f.calls.length, 0);
});

test("auth comes from config first, the .p8 stays a file path", async (t) => {
  const f = fixture(t, { state: readyState() });
  const seen = [];
  await createAppleProfile({ ...f.options, config: { appStoreConnect: { keyId: "CONFIGKEY", issuer: "config-issuer", keyPath: path.basename(f.keyPath) } }, env: {}, jwtSigner: (auth) => { seen.push(auth); return "synthetic-jwt"; }, dryRun: true });
  assert.deepEqual(seen, [{ keyPath: f.keyPath, keyId: "CONFIGKEY", issuer: "config-issuer" }]);
});

test("an existing profile that does not cover the devices is a name conflict, not a duplicate POST", async (t) => {
  const state = readyState();
  state.profiles = [{ id: "PROFILE-1", attributes: { name: NAME, profileState: "INVALID", expirationDate: "2030-01-01T00:00:00Z" }, rels: { bundleId: { id: "BUNDLE-1" }, certificates: [{ id: "CERT-1" }], devices: [] } }];
  const f = fixture(t, { state });
  await assert.rejects(createAppleProfile({ ...f.options, dryRun: false }), { code: "APPLE_PROFILE_NAME_CONFLICT" });
  assert.equal(mutations(f.calls).length, 0);
});

test("--register-device registers a missing Mac and includes it in the profile", async (t) => {
  const f = fixture(t, { state: readyState(), decoded: issued({ ProvisionedDevices: [UDID, "NEW-UDID-1234"] }) });
  await createAppleProfile({ ...f.options, registerDevices: ["new-udid-1234"], dryRun: false });
  const post = f.calls.find((c) => c.method === "POST" && c.path === "/v1/profiles");
  assert.deepEqual(post.body.data.relationships.devices.data.map((d) => d.id), ["DEV-1", "DEV-NEW"]);
});

test("a real run off macOS is refused before any call", async (t) => {
  const f = fixture(t);
  await assert.rejects(createAppleProfile({ ...f.options, dryRun: false, platform: "win32" }), { code: "APPLE_PROFILE_PLATFORM" });
  assert.equal(f.calls.length, 0);
});

test("CSR is a signed PKCS#10 request and the manual steps name every container", () => {
  const keys = generateKeyPairSync("rsa", { modulusLength: 2048 });
  const csr = buildCsr("Test Apple Development", keys);
  assert.equal(csr.der[0], 0x30);
  assert.ok(csr.der.includes(Buffer.from("Test Apple Development")));
  assert.equal(verify("sha256", csr.info, keys.publicKey, csr.signature), true);
  assert.match(csr.pem, /^-----BEGIN CERTIFICATE REQUEST-----\n[\s\S]+\n-----END CERTIFICATE REQUEST-----\n$/);
  assert.doesNotMatch(csr.pem, /PRIVATE/);
  assert.match(containerManualSteps(BUNDLE, ["iCloud.a", "iCloud.b"]), /iCloud\.a[\s\S]*iCloud\.b/);
});

test("argument parser rejects wildcard bundle ids, bad containers, unsupported types and unknown flags", () => {
  const base = ["--bundle-id", BUNDLE, "--name", NAME, "--icloud-container", CONTAINER, "--out", "x"];
  assert.throws(() => parseCreateArgs(["--bundle-id", "app.*", ...base.slice(2)]), { code: "APPLE_PROFILE_USAGE" });
  assert.throws(() => parseCreateArgs([...base, "--type", "MAC_APP_DIRECT"]), { code: "APPLE_PROFILE_USAGE" });
  assert.throws(() => parseCreateArgs([...base.slice(0, 4), "--icloud-container", "nope", "--out", "x"]), { code: "APPLE_PROFILE_USAGE" });
  assert.throws(() => parseCreateArgs([...base, "--bogus"]), { code: "APPLE_PROFILE_USAGE" });
  assert.throws(() => parseCreateArgs(base.slice(0, 6)), { code: "APPLE_PROFILE_USAGE" });
  assert.deepEqual(parseCreateArgs([...base, "--icloud-container", "iCloud.second"]).containers, [CONTAINER, "iCloud.second"]);
  assert.equal(parseCreateArgs(base).certificate, "create");
});

test("main --help describes the dry-run default without touching ASC", async () => {
  const lines = [];
  await main(["--help"], { log: (v) => lines.push(v) });
  assert.match(lines.join("\n"), /Default is --dry-run/);
});

const releaseCertificate = "MIIDizCCAnOgAwIBAgIUOVtNhnSONprpVGyOy5KS4rF7WawwDQYJKoZIhvcNAQELBQAwVTE+MDwGA1UEAww1RGV2ZWxvcGVyIElEIEFwcGxpY2F0aW9uOiBBZHJpYW4gRCdzb3V6YSAoNktMR0QzTExLRikxEzARBgNVBAsMCjZLTEdEM0xMS0YwHhcNMjYxMDA4MDY0NzExWhcNMzYxMDA1MDY0NzExWjBVMT4wPAYDVQQDDDVEZXZlbG9wZXIgSUQgQXBwbGljYXRpb246IEFkcmlhbiBEJ3NvdXphICg2S0xHRDNMTEtGKTETMBEGA1UECwwKNktMR0QzTExLRjCCASIwDQYJKoZIhvcNAQEBBQADggEPADCCAQoCggEBAJhlP4N8VQKkkHb7DA6vupCpMfdcN7cSXxo58PxZs0aVx+wGm+UIcTWoVzOtc+A2j1Jt+XCu2kum+fzeem9N0qWbxbLpMZD/2nh2EixN7cKtHIi+BM+1K1RGLWNQ7SHov36k6xUFueeWAkgs/h415yB/Wvl3m9htBjy2VnNpykEN1xoZ67WofhC39gxgP016xxFHHGHuieefPCkZ32xlc8dIfeWKlaiIzmJ1QSTIVQob94NKnehn+17S6uuSNxxt3sr+5Pp9vmHAqE3rzbVeLmwo1HP3jaduXpZxE1WgAlS0i9P4ZIrwvqU+zyQ8o1Ry9Sukq7KhFJPEivZIyfQLO7sCAwEAAaNTMFEwHQYDVR0OBBYEFD+GLnlvn6meULbXJax7aj4cQXUcMB8GA1UdIwQYMBaAFD+GLnlvn6meULbXJax7aj4cQXUcMA8GA1UdEwEB/wQFMAMBAf8wDQYJKoZIhvcNAQELBQADggEBAEvWu0nBG5Y3GGqSBp3JAjfcDsZ6xtaDbidWHuBWjrCMOGBSu4uGf0ytWGRJo3G0BGBi8vnr1TwTOQ6Pe0deEoMyqycMqGYxBNF8xhblBJ2aWLRyJPe64NNlwbvKZTBrnJuqOVVKLAwxixgbFE6LYj+QODBxIy2z7dAPBcpLuIiBHcVxncKIteoZRyxOqvq47z8QdEFUqo4XruqkkZ97BajLwkpfoMnM57gXQglvNWPOFsmXS26PV1K0LEsCu90VXyRs9SVaSrWkBvuTo1n55+/OLyLNYPTLJhkF8a9XyXQWTBcL+qpWDDv+4Js+PoNTyv2IDu8oiDQ/JvXTAv6lhHc=";
const TEAM = "6KLGD3LLKF", DIRECT_BUNDLE = "org.rightkit.qualification.credentials", DIRECT_NAME = "RightKit Qualification Credentials Direct";
const GROUP = `${TEAM}.${DIRECT_BUNDLE}`;
const directIssued = () => ({ Name: DIRECT_NAME, TeamIdentifier: [TEAM], Platform: ["OSX"], ProvisionsAllDevices: true, ExpirationDate: "2030-01-01T00:00:00Z", DeveloperCertificates: [releaseCertificate],
  // Apple-issued MAC_APP_DIRECT profile W7AVF2AK9C omits both debugging aliases.
  Entitlements: { "com.apple.application-identifier": `${TEAM}.${DIRECT_BUNDLE}`, "com.apple.developer.team-identifier": TEAM, "keychain-access-groups": [`${TEAM}.*`] } });
const directState = () => ({ bundles: [{ id: "BUNDLE-1", attributes: { identifier: DIRECT_BUNDLE, platform: "MAC_OS" } }], certificates: [{ id: "CERTDIRECT1", attributes: { certificateType: "DEVELOPER_ID_APPLICATION", serialNumber: new X509Certificate(Buffer.from(releaseCertificate, "base64")).serialNumber, expirationDate: "2030-01-01T00:00:00Z", certificateContent: releaseCertificate } }] });
function directFixture(t, options = {}) {
  const f = fixture(t, { state: directState(), decoded: directIssued(), localCertificate: releaseCertificate, ...options });
  f.options = { ...f.options, bundleId: DIRECT_BUNDLE, name: DIRECT_NAME, containers: [], type: "MAC_APP_DIRECT", certificate: "CERTDIRECT1", expectedTeam: TEAM, expectedKeychainGroup: GROUP };
  return f;
}
const directArgs = () => ["--bundle-id", DIRECT_BUNDLE, "--name", DIRECT_NAME, "--type", "MAC_APP_DIRECT", "--certificate", "CERTDIRECT1", "--expected-team", TEAM, "--expected-keychain-group", GROUP, "--out", "Direct.provisionprofile"];

test("direct args require explicit team, exact group & existing certificate; refuse development-only effects", async (t) => {
  assert.equal(parseCreateArgs(directArgs()).type, "MAC_APP_DIRECT");
  assert.equal(parseCreateArgs(directArgs()).dryRun, true);
  for (const extra of [["--certificate", "create"], ["--expected-team", "OTHER"], ["--expected-keychain-group", `${TEAM}.*`], ["--expected-keychain-group", "OTHERTEAMX.group"], ["--icloud-container", CONTAINER], ["--register-device", UDID], ["--key-out", "key.pem"]]) {
    assert.throws(() => parseCreateArgs([...directArgs(), ...extra]), { code: "APPLE_PROFILE_USAGE" });
  }
  for (const missing of ["--expected-team", "--expected-keychain-group", "--certificate"]) {
    const args = directArgs(), i = args.indexOf(missing); args.splice(i, 2);
    assert.throws(() => parseCreateArgs(args), { code: "APPLE_PROFILE_USAGE" });
  }
  const f = directFixture(t);
  for (const patch of [{ certificate: "create" }, { containers: [CONTAINER] }, { registerDevices: [UDID] }, { keyOut: "key.pem" }, { expectedTeam: undefined }, { expectedKeychainGroup: `${TEAM}.*` }]) {
    await assert.rejects(createAppleProfile({ ...f.options, ...patch }), { code: "APPLE_PROFILE_USAGE" });
  }
  assert.equal(f.calls.length, 0);
});

test("direct dry run discovers with GET only & plans only explicit bundle plus profile", async (t) => {
  const f = directFixture(t, { state: { ...directState(), bundles: [] } });
  const result = await createAppleProfile(f.options);
  assert.equal(mutations(f.calls).length, 0);
  assert.deepEqual(result.planned.map((entry) => entry.path), ["/v1/bundleIds", "/v1/profiles"]);
  assert.deepEqual(Object.keys(result.planned[1].body.data.relationships).sort(), ["bundleId", "certificates"]);
  assert.ok(!f.calls.some((entry) => /devices|Capabilities/.test(entry.path)));
  assert.deepEqual(readdirSync(f.root), ["AuthKey_SYNTHETIC.p8"]);
});

test("direct apply creates only profile, verifies issuer wildcard authorization & writes CMS", async (t) => {
  const f = directFixture(t);
  const result = await createAppleProfile({ ...f.options, dryRun: false });
  assert.deepEqual(mutations(f.calls).map((entry) => `${entry.method} ${entry.path}`), ["POST /v1/profiles"]);
  assert.deepEqual(mutations(f.calls)[0].body, { data: { type: "profiles", attributes: { name: DIRECT_NAME, profileType: "MAC_APP_DIRECT" }, relationships: { bundleId: { data: { type: "bundleIds", id: "BUNDLE-1" } }, certificates: { data: [{ type: "certificates", id: "CERTDIRECT1" }] } } } });
  assert.equal(result.expectedKeychainGroup, GROUP);
  assert.equal(readFileSync(f.out, "utf8"), "issued CMS bytes");
  assert.deepEqual(f.local.filter(([command]) => command === "security").map(([, verb]) => verb), ["find-identity", "find-certificate", "cms"]);
  if (process.platform !== "win32") assert.equal(statSync(f.out).mode & 0o777, 0o600);
});

test("direct apply may register only explicit missing bundle & dedicated profile", async (t) => {
  const f = directFixture(t, { state: { ...directState(), bundles: [] } });
  await createAppleProfile({ ...f.options, dryRun: false });
  assert.deepEqual(mutations(f.calls).map((entry) => `${entry.method} ${entry.path}`), ["POST /v1/bundleIds", "POST /v1/profiles"]);
  assert.equal(mutations(f.calls)[0].body.data.attributes.identifier, DIRECT_BUNDLE);
});

test("direct accepts Developer ID Application G2 with matching local identity & rejects KEXT G2", async (t) => {
  const state = directState(); state.certificates[0].attributes.certificateType = "DEVELOPER_ID_APPLICATION_G2";
  const f = directFixture(t, { state });
  const result = await createAppleProfile({ ...f.options, dryRun: false });
  assert.equal(result.certificate.id, "CERTDIRECT1");
  assert.equal(readFileSync(f.out, "utf8"), "issued CMS bytes");

  const kextState = directState(); kextState.certificates[0].attributes.certificateType = "DEVELOPER_ID_KEXT_G2";
  const g = directFixture(t, { state: kextState });
  await assert.rejects(createAppleProfile({ ...g.options, dryRun: false }), { code: "APPLE_PROFILE_CERT_TYPE" });
  assert.equal(mutations(g.calls).length, 0);
  assert.deepEqual(readdirSync(g.root), ["AuthKey_SYNTHETIC.p8"]);
});

const existingDirectProfile = () => ({ id: "PROFILE-1", attributes: { name: DIRECT_NAME, profileType: "MAC_APP_DIRECT", profileState: "ACTIVE", expirationDate: "2030-01-01T00:00:00Z", profileContent: Buffer.from("issued CMS bytes").toString("base64") }, rels: { bundleId: { id: "BUNDLE-1" }, certificates: [{ id: "CERTDIRECT1" }] } });
test("direct active named profile reuses matching bundle & sole certificate without device lookup", async (t) => {
  const f = directFixture(t, { state: { ...directState(), profiles: [existingDirectProfile()] } });
  const result = await createAppleProfile({ ...f.options, dryRun: false });
  assert.equal(result.profile.action, "reuse");
  assert.equal(mutations(f.calls).length, 0);
  assert.ok(!f.calls.some((entry) => /devices|Capabilities/.test(entry.path)));
});

test("direct conflicting names fail before mutations", async (t) => {
  for (const patch of [{ attributes: { profileType: "MAC_APP_DEVELOPMENT" } }, { attributes: { profileState: "INVALID" } }, { attributes: { expirationDate: "2000-01-01" } }, { rels: { bundleId: { id: "OTHER" } } }, { rels: { certificates: [{ id: "OTHER" }] } }, { rels: { certificates: [{ id: "CERTDIRECT1" }, { id: "OTHER" }] } }]) {
    const p = existingDirectProfile(); p.attributes = { ...p.attributes, ...patch.attributes }; p.rels = { ...p.rels, ...patch.rels };
    const f = directFixture(t, { state: { ...directState(), profiles: [p] } });
    await assert.rejects(createAppleProfile({ ...f.options, dryRun: false }), { code: "APPLE_PROFILE_NAME_CONFLICT" });
    assert.equal(mutations(f.calls).length, 0);
  }
  const f = directFixture(t, { state: { ...directState(), profiles: [existingDirectProfile(), { ...existingDirectProfile(), id: "PROFILE-2" }] } });
  await assert.rejects(createAppleProfile(f.options), { code: "APPLE_PROFILE_NAME_CONFLICT" });
  assert.equal(mutations(f.calls).length, 0);
});

test("direct certificate selection fails closed before any write", async (t) => {
  for (const attributes of [{ certificateType: "DEVELOPMENT" }, { expirationDate: "2000-01-01" }, { certificateContent: "bad" }, { serialNumber: "00DEAD" }]) {
    const state = directState(); state.certificates[0].attributes = { ...state.certificates[0].attributes, ...attributes };
    const f = directFixture(t, { state });
    await assert.rejects(createAppleProfile({ ...f.options, dryRun: false }));
    assert.equal(mutations(f.calls).length, 0);
  }
  for (const options of [{ expectedTeam: "OTHERTEAMX", expectedKeychainGroup: "OTHERTEAMX.org.rightkit.qualification.credentials" }, { run: fixture(t).options.run }]) {
    const f = directFixture(t);
    await assert.rejects(createAppleProfile({ ...f.options, ...options, dryRun: false }), { code: "APPLE_PROFILE_SIGNING_CERT_MISSING" });
    assert.equal(mutations(f.calls).length, 0);
  }
});

test("direct decoded profile rejects wrong team, app, platform, expiry, debug, certificate & group", async (t) => {
  const base = directIssued();
  const patches = [{ Name: "Other" }, { TeamIdentifier: ["OTHERTEAMX"] }, { TeamIdentifier: [TEAM, "OTHERTEAMX"] }, { Platform: ["iOS"] }, { ExpirationDate: "2000-01-01" }, { ProvisionsAllDevices: false }, { DeveloperCertificates: [developmentCertificate] },
    ...[{ "com.apple.application-identifier": `${TEAM}.other` }, { "com.apple.application-identifier": `${TEAM}.*` }, { "application-identifier": "OTHER.app" }, { "com.apple.developer.team-identifier": "OTHERTEAMX" }, { "get-task-allow": true }, { "com.apple.security.get-task-allow": true }, { "keychain-access-groups": ["OTHERTEAMX.*"] }, { "keychain-access-groups": [`${TEAM}.other.*`] }, { "keychain-access-groups": ["*"] }].map((ent) => ({ Entitlements: { ...base.Entitlements, ...ent } }))];
  for (const patch of patches) {
    const f = directFixture(t, { decoded: { ...base, ...patch } });
    await assert.rejects(createAppleProfile({ ...f.options, dryRun: false }), { code: "APPLE_PROFILE_VERIFY_FAILED" });
    assert.deepEqual(readdirSync(path.dirname(f.out)), []);
    assert.ok(!f.calls.some((entry) => entry.method === "DELETE"));
  }
});

test("direct accepts exact keychain authorization without broadening app entitlement input", async (t) => {
  const decoded = directIssued(); decoded.Entitlements["keychain-access-groups"] = [GROUP];
  const f = directFixture(t, { decoded });
  await createAppleProfile({ ...f.options, dryRun: false });
  assert.equal(readFileSync(f.out, "utf8"), "issued CMS bytes");
});

test("direct debug allowlist accepts absence or false & rejects grants, malformed values & conflicting aliases", () => {
  const keys = ["get-task-allow", "com.apple.security.get-task-allow"];
  const options = { bundleId: DIRECT_BUNDLE, name: DIRECT_NAME, expectedTeam: TEAM, expectedKeychainGroup: GROUP,
    certificateSha1: createHash("sha1").update(Buffer.from(releaseCertificate, "base64")).digest("hex").toUpperCase(), now: new Date("2026-10-20T00:00:00Z") };
  const check = (debug) => {
    const decoded = directIssued();
    decoded.Entitlements = { ...decoded.Entitlements, ...debug };
    return () => verifyDirectProfile(decoded, options);
  };
  for (const debug of [{}, { [keys[0]]: false }, { [keys[1]]: false }, { [keys[0]]: false, [keys[1]]: false }]) {
    assert.doesNotThrow(check(debug), JSON.stringify(debug));
  }
  for (const key of keys) {
    const other = keys.find((alias) => alias !== key);
    for (const value of [true, "false", "true", 0, 1, null, undefined, [], {}]) {
      for (const debug of [{ [key]: value }, { [key]: value, [other]: false }]) {
        assert.throws(check(debug), (error) => error.code === "APPLE_PROFILE_VERIFY_FAILED" && /debugging is not disabled/.test(error.message), `${key}=${String(value)}`);
      }
    }
  }
});
