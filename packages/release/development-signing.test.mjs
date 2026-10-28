import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { chmodSync, existsSync, mkdtempSync, mkdirSync, readFileSync, realpathSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { executeSigningRequest } from "./rightkit-signing-agent.mjs";
import { spawnSync } from "./background-process.mjs";
import { MacosDevelopmentSigningError, resolveMacosDevelopmentSigning, signMacosDevelopmentBundle } from "./development-signing.mjs";
import { runDevelopment } from "./development.mjs";

// Not run on Windows: development signing runs on macOS.
const WINDOWS_SKIP = process.platform === "win32" ? "development signing runs on macOS" : false;

// Public, self-signed fixture certificates only; no private signing material.
const developmentCertificate = "MIIDfTCCAmWgAwIBAgIUSdJGaBUVphuaRHDPPHdfv6wuYqcwDQYJKoZIhvcNAQELBQAwTjE3MDUGA1UEAwwuQXBwbGUgRGV2ZWxvcG1lbnQ6IEFkcmlhbiBEJ3NvdXphICg4NzU3S0w5ODlSKTETMBEGA1UECwwKODc1N0tMOTg5UjAeFw0yNjEwMDgwNjQ3MTFaFw0zNjEwMDUwNjQ3MTFaME4xNzA1BgNVBAMMLkFwcGxlIERldmVsb3BtZW50OiBBZHJpYW4gRCdzb3V6YSAoODc1N0tMOTg5UikxEzARBgNVBAsMCjg3NTdLTDk4OVIwggEiMA0GCSqGSIb3DQEBAQUAA4IBDwAwggEKAoIBAQDPkONHjVz4oJf4U8YeYUrL9wXVmUUOeh5yrsVsXxqWypQw1l0Fl9/fRHpTJ7DNxUc56Vpm39LNnRXeCJcOEN7V7baUedWNQYSDXjyDEvoHX1g4BUmswRGblNvjS17qLItO8H3LfovGFSlVTfsjgp/liH0vHx1msRxLUKpdsqFy29phk33RqBQJm28aaLglfaiTCkYKT9FcTmq0x+bhOK7hFIp059hqaIT06VKcXJ4Abmwiu7krL2Ni2m812NdPz4bSJim6890V9bXCnpH4A/1Goxp2a3KmpGtp/USZhtqgo+Vi2i6kVEmazOVtVWi3p1ee4iPYEWwGxhLaiH0pg2vfAgMBAAGjUzBRMB0GA1UdDgQWBBTg8C9vvsB3ZTVzuvZVJHPhMwA4lTAfBgNVHSMEGDAWgBTg8C9vvsB3ZTVzuvZVJHPhMwA4lTAPBgNVHRMBAf8EBTADAQH/MA0GCSqGSIb3DQEBCwUAA4IBAQDAxnLum9vCtTt939b8blQkec/qr3yv7yHkWIeCOnZU3cjKuzwOtavKeCrG3fmIIDYJlr6scAq77ua+Z6NDrQ13hrm99rUSJgQn6A0TXfbazKFcI5SlO11ZnHTN1XYjV9hxO3BMZSueUs3GBA+tofZ6s/rjk2WTTBUFNqmEUkhkF8f/YoWleIZFPdB6fofSEdqmLb5CBp5FVgWnb+MqkTtk3BkSx2KGNXOQUIGMCdch0gwuzA4fjDR572TrZc2/DBX9yO1W2im5RoLt6zmf/ZLguQQHBQLCm4TEcrHxou8DbSTPTb/hIuyDYi1uCoIEoxsiZ561QacFRQMMMi2CGfaA";
const releaseCertificate = "MIIDizCCAnOgAwIBAgIUOVtNhnSONprpVGyOy5KS4rF7WawwDQYJKoZIhvcNAQELBQAwVTE+MDwGA1UEAww1RGV2ZWxvcGVyIElEIEFwcGxpY2F0aW9uOiBBZHJpYW4gRCdzb3V6YSAoNktMR0QzTExLRikxEzARBgNVBAsMCjZLTEdEM0xMS0YwHhcNMjYxMDA4MDY0NzExWhcNMzYxMDA1MDY0NzExWjBVMT4wPAYDVQQDDDVEZXZlbG9wZXIgSUQgQXBwbGljYXRpb246IEFkcmlhbiBEJ3NvdXphICg2S0xHRDNMTEtGKTETMBEGA1UECwwKNktMR0QzTExLRjCCASIwDQYJKoZIhvcNAQEBBQADggEPADCCAQoCggEBAJhlP4N8VQKkkHb7DA6vupCpMfdcN7cSXxo58PxZs0aVx+wGm+UIcTWoVzOtc+A2j1Jt+XCu2kum+fzeem9N0qWbxbLpMZD/2nh2EixN7cKtHIi+BM+1K1RGLWNQ7SHov36k6xUFueeWAkgs/h415yB/Wvl3m9htBjy2VnNpykEN1xoZ67WofhC39gxgP016xxFHHGHuieefPCkZ32xlc8dIfeWKlaiIzmJ1QSTIVQob94NKnehn+17S6uuSNxxt3sr+5Pp9vmHAqE3rzbVeLmwo1HP3jaduXpZxE1WgAlS0i9P4ZIrwvqU+zyQ8o1Ry9Sukq7KhFJPEivZIyfQLO7sCAwEAAaNTMFEwHQYDVR0OBBYEFD+GLnlvn6meULbXJax7aj4cQXUcMB8GA1UdIwQYMBaAFD+GLnlvn6meULbXJax7aj4cQXUcMA8GA1UdEwEB/wQFMAMBAf8wDQYJKoZIhvcNAQELBQADggEBAEvWu0nBG5Y3GGqSBp3JAjfcDsZ6xtaDbidWHuBWjrCMOGBSu4uGf0ytWGRJo3G0BGBi8vnr1TwTOQ6Pe0deEoMyqycMqGYxBNF8xhblBJ2aWLRyJPe64NNlwbvKZTBrnJuqOVVKLAwxixgbFE6LYj+QODBxIy2z7dAPBcpLuIiBHcVxncKIteoZRyxOqvq47z8QdEFUqo4XruqkkZ97BajLwkpfoMnM57gXQglvNWPOFsmXS26PV1K0LEsCu90VXyRs9SVaSrWkBvuTo1n55+/OLyLNYPTLJhkF8a9XyXQWTBcL+qpWDDv+4Js+PoNTyv2IDu8oiDQ/JvXTAv6lhHc=";
const developmentIdentity = "Apple Development: Adrian D'souza (8757KL989R)";
const releaseIdentity = "Developer ID Application: Adrian D'souza (6KLGD3LLKF)";
const sha1 = (data) => createHash("sha1").update(Buffer.from(data, "base64")).digest("hex").toUpperCase();
const identities = `1) ${sha1(developmentCertificate)} "${developmentIdentity}"\n2) ${sha1(releaseCertificate)} "${releaseIdentity}"`;

function fixture(t, { profile = null, mac = {}, team = "6KLGD3LLKF", signingFailure = null, listing = identities } = {}) {
  const root = realpathSync(mkdtempSync(path.join(os.tmpdir(), "rightkit-dev-sign-test-")));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const bundle = path.join(root, "bundle/Fixture.app");
  const write = (file, contents) => { mkdirSync(path.dirname(file), { recursive: true }); writeFileSync(file, contents); };
  // The fake plutil returns file content verbatim for `-convert json`, so fixture plists are JSON (real plutil reads either format).
  write(path.join(bundle, "Contents/Info.plist"), JSON.stringify({ CFBundleExecutable: "fixture", CFBundleIdentifier: "app.fixture" }));
  write(path.join(bundle, "Contents/MacOS/fixture"), Buffer.from("cffaedfe00000000", "hex"));
  write(path.join(root, "src-tauri/tauri.conf.json"), JSON.stringify({ bundle: { macOS: { signingIdentity: releaseIdentity, entitlements: "entitlements.plist", ...mac } } }));
  const entitlements = { "com.apple.security.cs.allow-jit": true, "com.apple.developer.icloud-services": ["CloudKit"] };
  write(path.join(root, "src-tauri/entitlements.plist"), JSON.stringify(entitlements));
  if (profile) write(path.join(root, "src-tauri/profile.provisionprofile"), "profile bytes");
  const calls = [];
  const run = (command, args, options) => {
    calls.push({ command, args, options });
    if (command === "/bin/launchctl") return { status: 0, stdout: "Aqua\n" };
    if (command === "security" && args[0] === "find-identity") return { status: 0, stdout: listing };
    if (command === "security" && args[0] === "cms") return { status: 0, stdout: "mock CMS plist" };
    if (command === "python3") return { status: 0, stdout: JSON.stringify(profile) };
    if (command === "plutil") return { status: 0, stdout: args[1] === "json" ? readFileSync(args.at(-1), "utf8") : "" };
    if (command === "codesign" && args.includes("--sign") && signingFailure) return signingFailure;
    if (command === "codesign" && args.includes("--verbose=4")) return { status: 0, stderr: `Authority=${releaseIdentity}\nTeamIdentifier=${team}\n` };
    if (command === "codesign" && args.includes("-r-")) return { status: 0, stdout: `designated => identifier "app.fixture" and anchor apple generic and certificate leaf[subject.OU] = "${team}"\n` };
    return { status: 0, stdout: "", stderr: "" };
  };
  // Build steps run as given (the build door is build-launch.test.mjs's subject; here a stand-in build only produces the bundle).
  return { root, bundle, descriptor: { manifestPath: "src-tauri/Cargo.toml" }, env: {}, run, door: (command, args) => ({ command, args }), calls, write };
}

function profile(certificate = developmentCertificate, team = "8757KL989R") {
  return { TeamIdentifier: [team], Platform: ["OSX"], ExpirationDate: "2099-01-01T00:00:00", DeveloperCertificates: [certificate], Entitlements: {} };
}

test("no profile uses configured release identity, entitlements and expected Team", (t) => {
  const f = fixture(t);
  const selected = resolveMacosDevelopmentSigning(f);
  assert.equal(selected.identity, releaseIdentity);
  assert.equal(selected.teamIdentifier, "6KLGD3LLKF");
  assert.equal(selected.entitlements, path.join(f.root, "src-tauri/entitlements.plist"));
  assert.equal(selected.provisioningProfile, null);
  assert.equal(resolveMacosDevelopmentSigning({ ...f, env: { APPLE_TEAM_ID: "" } }).teamIdentifier, "6KLGD3LLKF");
});

test("unconfigured app uses release-style Developer ID discovery", (t) => {
  const f = fixture(t, { mac: { signingIdentity: null } });
  assert.equal(resolveMacosDevelopmentSigning(f).identity, releaseIdentity);
});

test("an Apple Development profile is refused: dev signs with Developer ID only", (t) => {
  const f = fixture(t, { profile: profile(), mac: { provisioningProfile: "profile.provisionprofile" } });
  assert.throws(() => resolveMacosDevelopmentSigning(f), (error) => error.code === "DEV_MAC_SIGNING_DEVELOPER_ID_REQUIRED" && /Developer ID provisioning profile/.test(error.message));
  assert.equal(f.calls.filter((call) => call.command === "codesign").length, 0);
});

test("Developer ID profile selects its authorized release certificate", (t) => {
  const f = fixture(t, { profile: profile(releaseCertificate, "6KLGD3LLKF"), mac: { provisioningProfile: "profile.provisionprofile" } });
  assert.equal(resolveMacosDevelopmentSigning(f).identity, releaseIdentity);
});

test("embedded app profile also governs dev signing", (t) => {
  const f = fixture(t, { profile: profile(releaseCertificate, "6KLGD3LLKF") });
  f.write(path.join(f.bundle, "Contents/embedded.provisionprofile"), "prepared profile");
  assert.equal(resolveMacosDevelopmentSigning(f).identity, releaseIdentity);
});

test("profile TeamIdentifier governs and a development certificate is still refused", (t) => {
  const f = fixture(t, { profile: profile(developmentCertificate, "6KLGD3LLKF"), mac: { provisioningProfile: "profile.provisionprofile" } });
  assert.throws(() => resolveMacosDevelopmentSigning(f), (error) => error.code === "DEV_MAC_SIGNING_DEVELOPER_ID_REQUIRED");
});

test("identity env overrides cannot select an Apple Development certificate", (t) => {
  const f = fixture(t, { profile: profile(releaseCertificate, "6KLGD3LLKF"), mac: { provisioningProfile: "profile.provisionprofile" } });
  assert.equal(resolveMacosDevelopmentSigning({ ...f, env: { APPLE_SIGNING_IDENTITY: releaseIdentity } }).identity, releaseIdentity);
  assert.throws(() => resolveMacosDevelopmentSigning({ ...f, env: { APPLE_DEVELOPER_ID: developmentIdentity } }), (error) => error.code === "DEV_MAC_SIGNING_DEVELOPER_ID_REQUIRED");
});

test("missing Developer ID identity fails with typed error naming configured identity", (t) => {
  const f = fixture(t, { listing: "0 valid identities found" });
  assert.throws(() => signMacosDevelopmentBundle(f), (error) => error instanceof MacosDevelopmentSigningError && error.code === "DEV_MAC_SIGNING_DEVELOPER_ID_REQUIRED" && error.message.includes(releaseIdentity));
  assert.equal(f.calls.filter((call) => call.command === "codesign").length, 0);
});

test("ad hoc configuration and overrides are rejected without signing", (t) => {
  const f = fixture(t, { mac: { signingIdentity: "-" } });
  for (const env of [{}, { APPLE_SIGNING_IDENTITY: "-" }, { APPLE_DEVELOPER_ID: "-" }]) {
    assert.throws(() => signMacosDevelopmentBundle({ ...f, env }), (error) => error.code === "DEV_MAC_SIGNING_IDENTITY_INVALID");
  }
  assert.equal(f.calls.length, 0);
});

const ciEnv = { GITHUB_ACTIONS: "true", CI: "true", RUNNER_ENVIRONMENT: "github-hosted" };
function adhocRun(f) {
  return (command, args, options) => command === "codesign" && args.includes("--verbose=4")
    ? (f.run(command, args, options), { status: 0, stderr: "Signature=adhoc\nTeamIdentifier=not set\n" })
    : f.run(command, args, options);
}

test("ci-adhoc opt-in is refused off a GitHub-hosted runner, by config and by env, without signing", (t) => {
  const f = fixture(t);
  const hosts = [{}, { GITHUB_ACTIONS: "true" }, { CI: "true" }, { GITHUB_ACTIONS: "true", CI: "true" }, { ...ciEnv, RUNNER_ENVIRONMENT: "self-hosted" }, { ...ciEnv, GITHUB_ACTIONS: "false" }];
  for (const host of hosts) {
    assert.throws(() => signMacosDevelopmentBundle({ ...f, descriptor: { ...f.descriptor, signing: "ci-adhoc" }, env: host }), (error) => error.code === "DEV_MAC_SIGNING_CI_ADHOC_REFUSED");
    assert.throws(() => signMacosDevelopmentBundle({ ...f, env: { ...host, RIGHTKIT_DEV_SIGNING: "ci-adhoc" } }), (error) => error.code === "DEV_MAC_SIGNING_CI_ADHOC_REFUSED");
  }
  assert.equal(f.calls.length, 0);
});

test("unknown signing mode values are refused, never treated as the default", (t) => {
  const f = fixture(t);
  assert.throws(() => signMacosDevelopmentBundle({ ...f, descriptor: { ...f.descriptor, signing: "adhoc" }, env: ciEnv }), (error) => error.code === "DEV_MAC_SIGNING_MODE_INVALID");
  assert.throws(() => signMacosDevelopmentBundle({ ...f, env: { ...ciEnv, RIGHTKIT_DEV_SIGNING: "yes" } }), (error) => error.code === "DEV_MAC_SIGNING_MODE_INVALID");
});

test("a CI runner without the opt-in still requires a Keychain identity and never falls back to ad hoc", (t) => {
  const f = fixture(t, { listing: "0 valid identities found" });
  assert.throws(() => signMacosDevelopmentBundle({ ...f, env: ciEnv }), (error) => error.code === "DEV_MAC_SIGNING_DEVELOPER_ID_REQUIRED");
  assert.equal(f.calls.filter((call) => call.command === "codesign").length, 0);
});

test("ci-adhoc on a CI runner signs ad hoc without Keychain, verifies, skips Team/requirement checks and marks the receipt", (t) => {
  for (const via of ["config", "env"]) {
    const f = fixture(t, { listing: "0 valid identities found", profile: profile(), mac: { provisioningProfile: "profile.provisionprofile" } });
    const options = via === "config" ? { ...f, descriptor: { ...f.descriptor, signing: "ci-adhoc" }, env: ciEnv } : { ...f, env: { ...ciEnv, RIGHTKIT_DEV_SIGNING: "ci-adhoc" } };
    const result = signMacosDevelopmentBundle({ ...options, run: adhocRun(f) });
    assert.ok(!f.calls.some((call) => call.command === "security"), "no Keychain lookup");
    const signs = f.calls.filter((call) => call.command === "codesign" && call.args.includes("--sign"));
    assert.ok(signs.length >= 1 && signs.every((call) => call.args[call.args.indexOf("--sign") + 1] === "-"));
    assert.ok(f.calls.some((call) => call.command === "codesign" && call.args.join(" ").startsWith("--verify --strict --deep")));
    assert.ok(!f.calls.some((call) => call.command === "codesign" && call.args.includes("-r-")), "no designated-requirement check");
    assert.equal(existsSync(path.join(f.bundle, "Contents/embedded.provisionprofile")), false);
    assert.equal(result.adhoc, true);
    assert.equal(result.identity, "-");
    assert.equal(result.mode, "ci-adhoc");
    assert.equal(result.releaseArtifact, false);
    assert.equal(result.teamIdentifier, null);
    assert.deepEqual({ ...result.receipt, note: undefined }, { signing: "ci-adhoc", signature: "adhoc", releaseArtifact: false, note: undefined });
  }
});

test("ci-adhoc applies only ad hoc safe entitlements and refuses a non-adhoc result", (t) => {
  const f = fixture(t);
  const parentEntitlements = [];
  const run = (command, args, options) => {
    if (command === "codesign" && args.includes("--sign") && args.at(-1) === f.bundle) parentEntitlements.push(JSON.parse(readFileSync(args[args.indexOf("--entitlements") + 1], "utf8")));
    return adhocRun(f)(command, args, options);
  };
  signMacosDevelopmentBundle({ ...f, env: { ...ciEnv, RIGHTKIT_DEV_SIGNING: "ci-adhoc" }, run });
  assert.deepEqual(parentEntitlements, [{ "com.apple.security.cs.allow-jit": true }]);
  assert.throws(() => signMacosDevelopmentBundle({ ...f, env: { ...ciEnv, RIGHTKIT_DEV_SIGNING: "ci-adhoc" } }), (error) => error.code === "DEV_MAC_SIGNATURE_INVALID");
});

test("profile embedding precedes inside-out nested code and parent sealing; no deep signing or ad hoc", { skip: WINDOWS_SKIP }, (t) => {
  const f = fixture(t, { profile: profile(releaseCertificate, "6KLGD3LLKF"), mac: { provisioningProfile: "profile.provisionprofile" } });
  const helper = path.join(f.bundle, "Contents/Library/LoginItems/Helper.app");
  const framework = path.join(helper, "Contents/Frameworks/Nested.framework");
  const dylib = path.join(framework, "Versions/A/libNested.dylib");
  const sidecar = path.join(f.bundle, "Contents/MacOS/sidecar");
  f.write(path.join(helper, "Contents/Info.plist"), JSON.stringify({ CFBundleExecutable: "helper", CFBundleIdentifier: "app.fixture" }));
  f.write(path.join(helper, "Contents/MacOS/helper"), Buffer.from("cffaedfe00000000", "hex"));
  f.write(path.join(helper, "Contents/embedded.provisionprofile"), "profile bytes");
  f.write(dylib, Buffer.from("cffaedfe00000000", "hex"));
  f.write(sidecar, Buffer.from("cffaedfe00000000", "hex"));
  chmodSync(sidecar, 0o755);
  f.write(path.join(f.bundle, "Contents/Resources/model.bin"), "not code");
  if (process.platform !== "win32") symlinkSync(dylib, path.join(framework, "libNested.dylib"));
  const run = (command, args, options) => {
    if (command === "codesign" && args.includes("--sign")) assert.equal(readFileSync(path.join(f.bundle, "Contents/embedded.provisionprofile"), "utf8"), "profile bytes");
    if (command === "codesign" && args.includes("--sign") && args.at(-1) === sidecar) assert.deepEqual(JSON.parse(readFileSync(args[args.indexOf("--entitlements") + 1], "utf8")), { "com.apple.security.cs.allow-jit": true });
    return f.run(command, args, options);
  };
  const result = signMacosDevelopmentBundle({ ...f, run });
  const signs = f.calls.filter((call) => call.command === "codesign" && call.args.includes("--sign"));
  const targets = signs.map((call) => call.args.at(-1));
  assert.ok(targets.indexOf(dylib) < targets.indexOf(framework));
  assert.ok(targets.indexOf(framework) < targets.indexOf(helper));
  assert.ok(targets.indexOf(sidecar) < targets.indexOf(f.bundle));
  assert.equal(targets.at(-1), f.bundle);
  assert.equal(targets.length, 5);
  assert.ok(signs.every((call) => !call.args.includes("--deep") && call.args[call.args.indexOf("--sign") + 1] === sha1(releaseCertificate) && call.args.includes("runtime")));
  assert.equal(signs.at(-1).args[signs.at(-1).args.indexOf("--entitlements") + 1], path.join(f.root, "src-tauri/entitlements.plist"));
  assert.equal(signs.find((call) => call.args.at(-1) === helper).args.includes("--entitlements"), true);
  assert.ok(f.calls.some((call) => call.command === "codesign" && call.args.join(" ").startsWith("--verify --strict --deep")));
  assert.equal(result.teamIdentifier, "6KLGD3LLKF");
  assert.equal(result.verified, "strict-deep");
});

test("exit 36 fails with typed actionable key error and never retries ad hoc", (t) => {
  const f = fixture(t, { signingFailure: { status: 36, stderr: "errSecInteractionNotAllowed: User interaction is not allowed." } });
  assert.throws(() => signMacosDevelopmentBundle(f), (error) => error instanceof MacosDevelopmentSigningError && error.code === "DEV_MAC_SIGNING_KEY_UNAVAILABLE" && error.status === 36 && error.identity === releaseIdentity && /GUI session/.test(error.message) && /errSecInteractionNotAllowed/.test(error.message));
  assert.equal(f.calls.filter((call) => call.command === "codesign" && call.args.includes("--sign")).length, 1);
});

test("cdhash-bound designated requirement refuses installation", (t) => {
  const f = fixture(t);
  const run = (command, args, options) => command === "codesign" && args.includes("-r-") ? { status: 0, stdout: 'designated => cdhash H"abcdef"\n' } : f.run(command, args, options);
  assert.throws(() => signMacosDevelopmentBundle({ ...f, run }), (error) => error.code === "DEV_MAC_SIGNING_UNSTABLE_REQUIREMENT");
});

test("designated requirement is reported on a verified signing", (t) => {
  const f = fixture(t);
  assert.match(signMacosDevelopmentBundle(f).designatedRequirement, /^identifier "app\.fixture" and anchor apple generic/);
});

test("Team mismatch and missing TeamIdentifier refuse installation", (t) => {
  for (const team of ["WRONGTEAM", "not set"]) {
    const f = fixture(t, { team });
    assert.throws(() => signMacosDevelopmentBundle(f), (error) => error.code === "DEV_MAC_SIGNING_TEAM_MISMATCH");
  }
});

test("configured missing profile fails without signing", (t) => {
  const f = fixture(t, { mac: { provisioningProfile: "missing.provisionprofile" } });
  assert.throws(() => signMacosDevelopmentBundle(f), (error) => error.code === "DEV_MAC_SIGNING_PROFILE_INVALID");
  assert.equal(f.calls.length, 0);
});

test("dev signing failure leaves installed app intact and cleans staging", { skip: process.platform !== "darwin" }, (t) => {
  const f = fixture(t, { signingFailure: { status: 36, stderr: "errSecInteractionNotAllowed" } });
  const git = spawnSync("git", ["init", "-b", "main"], { cwd: f.root, encoding: "utf8" });
  assert.equal(git.status, 0, git.stderr);
  const destination = path.join(f.root, "installed/Fixture.app");
  f.write(path.join(destination, "Contents/Info.plist"), "previous app");
  const config = { development: { targets: { mac: { build: [{ cmd: "mock-build" }], install: { kind: "tauri", productName: "Fixture", bundleDirectory: "bundle", destination, targetTriple: "aarch64-apple-darwin" } } } } };
  assert.throws(() => runDevelopment({ ...f, platform: "mac", config, codesignEnvironment: (env) => env }), (error) => error.code === "DEV_MAC_SIGNING_KEY_UNAVAILABLE");
  assert.equal(readFileSync(path.join(destination, "Contents/Info.plist"), "utf8"), "previous app");
  assert.equal(existsSync(`${destination}.staging`), false);
  assert.equal(existsSync(`${destination}.previous`), false);
});

test("nested helper and sidecar use existing named entitlement variants", (t) => {
  const f = fixture(t);
  const credentials = path.join(f.bundle, "Contents/Library/LoginItems/FixtureAppleCredentials.app");
  f.write(path.join(credentials, "Contents/Info.plist"), JSON.stringify({ CFBundleExecutable: "credentials", CFBundleIdentifier: "app.fixture" }));
  f.write(path.join(credentials, "Contents/MacOS/credentials"), Buffer.from("cffaedfe00000000", "hex"));
  const engine = path.join(f.bundle, "Contents/MacOS/fixture-engine");
  f.write(engine, Buffer.from("cffaedfe00000000", "hex"));
  f.write(path.join(f.root, "src-tauri/entitlements-credentials.plist"), JSON.stringify({ "keychain-access-groups": ["6KLGD3LLKF.app.fixture.shared"] }));
  f.write(path.join(f.root, "src-tauri/entitlements-engine.plist"), JSON.stringify({ "com.apple.security.cs.allow-jit": true }));
  signMacosDevelopmentBundle(f);
  for (const [target, variant] of [[credentials, "credentials"], [engine, "engine"]]) {
    const sign = f.calls.find((call) => call.command === "codesign" && call.args.includes("--sign") && call.args.at(-1) === target);
    assert.equal(sign.args[sign.args.indexOf("--entitlements") + 1], path.join(f.root, `src-tauri/entitlements-${variant}.plist`));
  }
});

test("existing nested entitlements are preserved when no file variant applies", (t) => {
  const f = fixture(t);
  const sidecar = path.join(f.bundle, "Contents/MacOS/tool");
  f.write(sidecar, Buffer.from("cffaedfe00000000", "hex"));
  signMacosDevelopmentBundle({ ...f, run: (command, args, options) => {
    const result = f.run(command, args, options);
    return command === "codesign" && args.includes("--entitlements") ? { status: 0, stdout: '<plist><dict><key>existing</key><true/></dict></plist>' } : result;
  } });
  const sign = f.calls.find((call) => call.command === "codesign" && call.args.includes("--sign") && call.args.at(-1) === sidecar);
  assert.ok(sign.args.includes("--preserve-metadata=entitlements"));
});

test("Tauri files mapping embeds profile without a separate provisioning setting", (t) => {
  const f = fixture(t, { profile: profile(releaseCertificate, "6KLGD3LLKF"), mac: { files: { "embedded.provisionprofile": "profile.provisionprofile" } } });
  signMacosDevelopmentBundle(f);
  assert.equal(readFileSync(path.join(f.bundle, "Contents/embedded.provisionprofile"), "utf8"), "profile bytes");
});

test("SSH development signing keeps inside-out plan inside agent & verifies Team from SSH", (t) => {
  const f = fixture(t);
  const sidecar = path.join(f.bundle, "Contents/MacOS/tool");
  f.write(sidecar, Buffer.from("cffaedfe00000000", "hex"));
  const agentCalls = [];
  const run = (command, args, options) => {
    if (command === "codesign") assert.ok(!args.includes("--sign"), "SSH must never sign directly");
    return f.run(command, args, options);
  };
  const sessionOptions = { allowedRoots: [f.root, os.tmpdir()], request(plan) {
    return executeSigningRequest(plan, { home: f.root, environmentPath: "/usr/bin", run(cmd, argv, opts) {
      assert.equal(cmd, "/usr/bin/codesign");
      agentCalls.push(argv);
      return f.run("codesign", argv, opts);
    } });
  } };
  const result = signMacosDevelopmentBundle({ ...f, run, sessionOptions, env: { SSH_CONNECTION: "fixture" } });
  assert.deepEqual(agentCalls.map((args) => args.at(-1)), [sidecar, f.bundle]);
  assert.equal(result.teamIdentifier, "6KLGD3LLKF");
  assert.equal(result.verified, "strict-deep");
  assert.deepEqual(f.calls.at(-3).args, ["--verify", "--strict", "--deep", f.bundle]);
  assert.deepEqual(f.calls.at(-2).args, ["--display", "--verbose=4", f.bundle]);
  assert.deepEqual(f.calls.at(-1).args, ["--display", "-r-", f.bundle]);
});

test("dev install records verified Team identity and preserves relative framework symlinks", { skip: process.platform !== "darwin" }, (t) => {
  const f = fixture(t, { profile: profile(releaseCertificate, "6KLGD3LLKF"), mac: { provisioningProfile: "profile.provisionprofile" } });
  assert.equal(spawnSync("git", ["init", "-b", "main"], { cwd: f.root }).status, 0);
  const framework = path.join(f.bundle, "Contents/Frameworks/Fixture.framework");
  f.write(path.join(framework, "Versions/A/Fixture"), Buffer.from("cffaedfe00000000", "hex"));
  f.write(path.join(framework, "Versions/A/Resources/Info.plist"), JSON.stringify({ CFBundleExecutable: "Fixture" }));
  symlinkSync("A", path.join(framework, "Versions/Current"));
  symlinkSync("Versions/Current/Resources", path.join(framework, "Resources"));
  symlinkSync("Versions/Current/Fixture", path.join(framework, "Fixture"));
  const destination = path.join(f.root, "installed/Fixture.app");
  const logs = [];
  t.mock.method(console, "log", (message) => logs.push(message));
  const config = { development: { targets: { mac: { build: [{ cmd: "mock-build" }], install: { kind: "tauri", productName: "Fixture", bundleDirectory: "bundle", destination, targetTriple: "aarch64-apple-darwin" } } } } };
  const result = runDevelopment({ ...f, platform: "mac", config, codesignEnvironment: (env) => env });
  assert.equal(result.signing.identity, releaseIdentity);
  assert.equal(result.signing.teamIdentifier, "6KLGD3LLKF");
  assert.equal(result.signing.provisioningProfile, path.join(destination, "Contents/embedded.provisionprofile"));
  assert.ok(logs.some((message) => message.includes(`identity=${releaseIdentity}`) && message.includes("TeamIdentifier=6KLGD3LLKF")));
  const targets = f.calls.filter((call) => call.command === "codesign" && call.args.includes("--sign")).map((call) => call.args.at(-1));
  assert.equal(targets.length, 2); // framework bundle then parent; never their primary executables.
  assert.equal(readFileSync(path.join(destination, "Contents/Frameworks/Fixture.framework/Fixture")).toString("hex"), "cffaedfe00000000");
});

// ---- per-binary identifiers, timestamp, per-path entitlements, identity assertions (bogusyogi/rightkit#345) ----
const MACHO = Buffer.from("cffaedfe00000000", "hex");
function engineFixture(t) {
  const f = fixture(t);
  f.write(path.join(f.bundle, "Contents/MacOS/fixture-engine"), MACHO);
  f.write(path.join(f.bundle, "Contents/MacOS/fixture-engine-helper"), MACHO);
  f.write(path.join(f.bundle, "Contents/Resources/runtime/yt-dlp"), MACHO);
  f.write(path.join(f.bundle, "Contents/Resources/runtime/libx.dylib"), MACHO);
  f.write(path.join(f.root, "src-tauri/entitlements.engine.plist"), JSON.stringify({ "com.apple.security.cs.allow-jit": true }));
  return f;
}
const signCalls = (f) => f.calls.filter((call) => call.command === "codesign" && call.args.includes("--sign"));
const signArgs = (f, file) => signCalls(f).find((call) => call.args.at(-1) === file).args;
const valueAfter = (args, flag) => args[args.indexOf(flag) + 1];
const withDescriptor = (f, extra, extraConfig = {}) => ({ ...f, descriptor: { ...f.descriptor, ...extra }, config: extraConfig });
const NL = String.fromCharCode(10);
const codesignReport = ({ identifier, team = "6KLGD3LLKF", authority = releaseIdentity }) => [`Identifier=${identifier}`, `Authority=${authority}`, "Authority=Developer ID Certification Authority", "Authority=Apple Root CA", `TeamIdentifier=${team}`, ""].join(NL);
// codesign prints its identity report on stderr, never stdout.
const dvvRun = (f, reports, fallback) => (command, args, options) => {
  if (command === "codesign" && args[0] === "-dvv") {
    f.calls.push({ command, args, options });
    const key = path.relative(f.bundle, args[1]).split(path.sep).join("/") || ".";
    return { status: 0, stdout: "", stderr: reports[key] ?? fallback ?? "" };
  }
  return f.run(command, args, options);
};

test("nothing declared: no --identifier, no --timestamp and no extra calls (existing apps are unchanged)", (t) => {
  const f = engineFixture(t);
  const result = signMacosDevelopmentBundle(f);
  for (const call of signCalls(f)) assert.ok(!call.args.includes("--identifier") && !call.args.includes("--timestamp") && !call.args.includes("--timestamp=none"));
  assert.ok(!f.calls.some((call) => call.args.includes("-dvv")));
  assert.equal(result.assertedIdentities, undefined);
});

test("identifiers map an exact path or glob to --identifier; an exact key beats a glob; the parent keeps its Info.plist identifier", (t) => {
  const f = engineFixture(t);
  signMacosDevelopmentBundle(withDescriptor(f, { identifiers: { "Contents/MacOS/fixture-engine*": "app.fixture.engine", "Contents/MacOS/fixture-engine": "app.fixture.engine.exact" } }));
  assert.equal(valueAfter(signArgs(f, path.join(f.bundle, "Contents/MacOS/fixture-engine")), "--identifier"), "app.fixture.engine.exact");
  assert.equal(valueAfter(signArgs(f, path.join(f.bundle, "Contents/MacOS/fixture-engine-helper")), "--identifier"), "app.fixture.engine");
  assert.ok(!signArgs(f, path.join(f.bundle, "Contents/Resources/runtime/yt-dlp")).includes("--identifier"), "no template, no identifier");
  assert.ok(!signArgs(f, f.bundle).includes("--identifier"));
  signMacosDevelopmentBundle(withDescriptor(f, { identifiers: { ".": "app.fixture" } }));
  assert.equal(valueAfter(signCalls(f).at(-1).args, "--identifier"), "app.fixture");
});

test("identifierTemplate names the remaining plain Mach-O files like ScrapeRight (<bundleId>.runtime.<relpath>) and GenRight (<basename>)", (t) => {
  const f = engineFixture(t);
  signMacosDevelopmentBundle(withDescriptor(f, { identifiers: { "Contents/MacOS/fixture-engine*": "app.fixture.engine" }, identifierTemplate: "<bundleId>.runtime.<relpath>" }));
  assert.equal(valueAfter(signArgs(f, path.join(f.bundle, "Contents/MacOS/fixture-engine")), "--identifier"), "app.fixture.engine");
  assert.equal(valueAfter(signArgs(f, path.join(f.bundle, "Contents/Resources/runtime/yt-dlp")), "--identifier"), "app.fixture.runtime.Contents.Resources.runtime.yt-dlp");
  assert.equal(valueAfter(signArgs(f, path.join(f.bundle, "Contents/Resources/runtime/libx.dylib")), "--identifier"), "app.fixture.runtime.Contents.Resources.runtime.libx.dylib");
  assert.ok(!signArgs(f, f.bundle).includes("--identifier"), "the template never names the bundle");
  f.calls.length = 0;
  signMacosDevelopmentBundle(withDescriptor(f, { identifierTemplate: "<bundleId>.runtime.<basename>" }));
  assert.equal(valueAfter(signArgs(f, path.join(f.bundle, "Contents/Resources/runtime/libx.dylib")), "--identifier"), "app.fixture.runtime.libx.dylib");
  // sign.identifiers/identifierTemplate apply to dev too, so one declaration keeps dev and release identifiers identical.
  f.calls.length = 0;
  signMacosDevelopmentBundle(withDescriptor(f, {}, { targets: { mac: { sign: { identifiers: { "Contents/MacOS/fixture-engine": "app.fixture.engine" }, identifierTemplate: "<bundleId>.rt.<basename>" } } } }));
  assert.equal(valueAfter(signArgs(f, path.join(f.bundle, "Contents/MacOS/fixture-engine")), "--identifier"), "app.fixture.engine");
  assert.equal(valueAfter(signArgs(f, path.join(f.bundle, "Contents/Resources/runtime/yt-dlp")), "--identifier"), "app.fixture.rt.yt-dlp");
});

test("install.timestamp adds --timestamp to every dev signing call and is otherwise absent", (t) => {
  const f = engineFixture(t);
  signMacosDevelopmentBundle(withDescriptor(f, { timestamp: true }));
  const calls = signCalls(f);
  assert.ok(calls.length >= 5 && calls.every((call) => call.args.includes("--timestamp") && call.args.indexOf("--timestamp") < call.args.indexOf("--sign")));
});

test("per-path entitlements apply to the declared paths, beat a <main>-<suffix>.plist variant, and leave other files alone", (t) => {
  const f = engineFixture(t);
  f.write(path.join(f.root, "src-tauri/entitlements-engine.plist"), JSON.stringify({ "com.apple.security.cs.allow-jit": true }));
  f.write(path.join(f.root, "src-tauri/custom.plist"), JSON.stringify({ "com.apple.security.cs.allow-unsigned-executable-memory": true }));
  const engine = path.join(f.bundle, "Contents/MacOS/fixture-engine");
  // Without a map the existing variant applies.
  signMacosDevelopmentBundle(f);
  assert.equal(valueAfter(signArgs(f, engine), "--entitlements"), path.join(f.root, "src-tauri/entitlements-engine.plist"));
  f.calls.length = 0;
  const map = { "Contents/MacOS/fixture-engine": "src-tauri/custom.plist", "Contents/Resources/runtime/yt-dlp": "src-tauri/entitlements.engine.plist" };
  signMacosDevelopmentBundle(withDescriptor(f, { entitlements: map }));
  assert.equal(valueAfter(signArgs(f, engine), "--entitlements"), path.join(f.root, "src-tauri/custom.plist"));
  assert.equal(valueAfter(signArgs(f, path.join(f.bundle, "Contents/Resources/runtime/yt-dlp")), "--entitlements"), path.join(f.root, "src-tauri/entitlements.engine.plist"));
  assert.ok(!signArgs(f, path.join(f.bundle, "Contents/Resources/runtime/libx.dylib")).includes("--entitlements"));
  assert.equal(valueAfter(signArgs(f, f.bundle), "--entitlements"), path.join(f.root, "src-tauri/entitlements.plist"), "the app keeps its own entitlements");
  // A string sign.entitlements is still the app file; an object is the per-path map.
  f.calls.length = 0;
  signMacosDevelopmentBundle(withDescriptor(f, {}, { targets: { mac: { sign: { entitlements: { "Contents/Resources/**": "src-tauri/entitlements.engine.plist" } } } } }));
  assert.equal(valueAfter(signArgs(f, path.join(f.bundle, "Contents/Resources/runtime/libx.dylib")), "--entitlements"), path.join(f.root, "src-tauri/entitlements.engine.plist"));
  assert.equal(valueAfter(signArgs(f, f.bundle), "--entitlements"), path.join(f.root, "src-tauri/entitlements.plist"));
  f.calls.length = 0;
  signMacosDevelopmentBundle(withDescriptor(f, {}, { targets: { mac: { sign: { entitlements: "src-tauri/custom.plist" } } } }));
  assert.equal(valueAfter(signArgs(f, f.bundle), "--entitlements"), path.join(f.root, "src-tauri/custom.plist"));
});

test("install.assert reads codesign -dvv (stderr), checks identifier, Team and authority, and accepts globs", (t) => {
  const f = engineFixture(t);
  const reports = {
    ".": codesignReport({ identifier: "app.fixture" }),
    "Contents/MacOS/fixture-engine": codesignReport({ identifier: "app.fixture.engine" }),
    "Contents/Resources/runtime/libx.dylib": codesignReport({ identifier: "app.fixture.runtime.libx.dylib" }),
  };
  const assertions = [
    { path: ".", identifier: "app.fixture", teamIdentifier: "6KLGD3LLKF", authority: releaseIdentity },
    { path: "Contents/MacOS/fixture-engine", identifier: "app.fixture.engine" },
    { path: "Contents/Resources/runtime/*.dylib", teamIdentifier: "6KLGD3LLKF" },
  ];
  const result = signMacosDevelopmentBundle({ ...withDescriptor(f, { assert: assertions }), run: dvvRun(f, reports) });
  assert.equal(result.assertedIdentities, 3);
  assert.ok(f.calls.filter((call) => call.args[0] === "-dvv").every((call) => call.command === "codesign"));
});

test("install.assert fails closed on a wrong identifier, Team or authority, and on a glob that matches nothing", (t) => {
  const f = engineFixture(t);
  const good = { ".": codesignReport({ identifier: "app.fixture" }), "Contents/MacOS/fixture-engine": codesignReport({ identifier: "app.fixture.engine" }) };
  const failing = (assertions, reports = good) => assert.throws(() => signMacosDevelopmentBundle({ ...withDescriptor(f, { assert: assertions }), run: dvvRun(f, reports) }), (error) => error.code === "DEV_MAC_SIGNING_ASSERT_FAILED" && /Refusing dev installation/.test(error.message));
  failing([{ path: "Contents/MacOS/fixture-engine", identifier: "app.fixture.wrong" }]);
  failing([{ path: ".", teamIdentifier: "OTHERTEAM" }]);
  failing([{ path: ".", authority: "Developer ID Application: Someone Else (AAAAAAAAAA)" }]);
  failing([{ path: "Contents/Frameworks/libonnxruntime*.dylib", teamIdentifier: "6KLGD3LLKF" }]);
  failing([{ path: "Contents/MacOS/fixture-engine", identifier: "app.fixture.engine" }], { ...good, "Contents/MacOS/fixture-engine": "" });
});

test("assertTeamSignedNested requires every Mach-O, bundled dylibs included, to carry the app's TeamIdentifier", (t) => {
  const f = engineFixture(t);
  f.write(path.join(f.bundle, "Contents/Frameworks/libonnxruntime.1.2.dylib"), MACHO);
  f.write(path.join(f.bundle, "Contents/Resources/model.bin"), "not code");
  const fallback = codesignReport({ identifier: "x" });
  const ok = signMacosDevelopmentBundle({ ...withDescriptor(f, { assertTeamSignedNested: true }), run: dvvRun(f, {}, fallback) });
  assert.equal(ok.assertedIdentities, 7, "the bundle plus its six Mach-O files; model.bin is not code");
  const foreign = { "Contents/Frameworks/libonnxruntime.1.2.dylib": codesignReport({ identifier: "libonnxruntime", team: "UBF8T346G9" }) };
  assert.throws(() => signMacosDevelopmentBundle({ ...withDescriptor(f, { assertTeamSignedNested: true }), run: dvvRun(f, foreign, fallback) }), (error) => error.code === "DEV_MAC_SIGNING_ASSERT_FAILED" && /libonnxruntime\.1\.2\.dylib: TeamIdentifier=UBF8T346G9/.test(error.message));
  const adhoc = { "Contents/Resources/runtime/yt-dlp": ["Identifier=yt-dlp", "Signature=adhoc", "TeamIdentifier=not set", ""].join(NL) };
  assert.throws(() => signMacosDevelopmentBundle({ ...withDescriptor(f, { assertTeamSignedNested: true }), run: dvvRun(f, adhoc, fallback) }), (error) => error.code === "DEV_MAC_SIGNING_ASSERT_FAILED" && /yt-dlp: ad hoc signed/.test(error.message));
});

test("malformed per-binary signing settings are refused before any signing call", (t) => {
  const f = engineFixture(t);
  const bad = [
    { identifiers: { "Contents/MacOS/x": "-evil" } },
    { identifiers: { "../x": "app.x" } },
    { identifiers: { "/abs": "app.x" } },
    { identifiers: [] },
    { identifierTemplate: "<bundleId>.<nope>" },
    { identifierTemplate: "<bundleId>.static" },
    { entitlements: { "Contents/MacOS/x": "src-tauri/missing.plist" } },
    { timestamp: "yes" },
    { assert: [{ path: "." }] },
    { assert: [{ path: ".", identifier: "a", extra: 1 }] },
    { assert: "no" },
    { assertTeamSignedNested: 1 },
  ];
  for (const extra of bad) assert.throws(() => signMacosDevelopmentBundle(withDescriptor(f, extra)), (error) => error.code === "DEV_MAC_SIGNING_CONFIG_INVALID", JSON.stringify(extra));
  assert.equal(f.calls.length, 0);
});
