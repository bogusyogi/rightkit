import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import nodeFs, { cpSync, existsSync, mkdirSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import path from "node:path";
import test from "node:test";
import { tempRoot } from "./test-support/temp-roots.mjs";
import {
  IOS_PACKAGE_STEPS, IOS_RECEIPT_NAME, IOS_SIGNING_CONTRACT, IosPackageError, archiveArgv, assertDistributionEntitlements, assertDistributionProfile,
  assertVersionIdentity, exportArgv, exportOptionsPlist, iosPackagePlan, iosSealEntry, nextDateBuildNumber, runIosPackageRecipe, validateIosPackageConfig,
  jsonSafePlistText, verifyIosReceipt, xcodegenArgv, assertTrustedPins, parsePackageResolved, swiftTrustFlags, IOS_EXPORT_OPTIONS_RELATIVE,
} from "./ios-package-recipe.mjs";
import { outputTail } from "./ios-host.mjs";
import { createNativeRunner } from "./build-launch.mjs";
import { managedRuntimeClient } from "./managed-roots.mjs";
import { NATIVE_ADAPTERS, nativeManagedOutputs, nativeStateDir, nativeToolOf } from "./native-adapters.mjs";

const TEAM = "6KLGD3LLKF";
const BUNDLE = "dev.orthic.pulse.ios";
const baseSpec = (over = {}) => ({
  project: "ios/Pulse.xcodeproj", projectSpec: "ios/project.yml", scheme: "Pulse", teamId: TEAM, bundleId: BUNDLE,
  archive: "build/ios/Pulse.xcarchive", ipa: "build/ios/export/Pulse.ipa", ...over,
});
const invalid = (spec, pattern) => assert.throws(() => validateIosPackageConfig(spec), (error) => error instanceof IosPackageError && error.code === "IOS_PACKAGE_CONFIG_INVALID" && pattern.test(error.message), String(pattern));
const slashed = (value) => String(value).replaceAll("\\", "/");
const NOW = new Date("2026-10-18T09:30:00Z");

// ---- config -------------------------------------------------------------------------------------------------------
test("a valid config normalizes with the documented defaults", () => {
  const spec = validateIosPackageConfig(baseSpec());
  assert.equal(spec.configuration, "Release");
  assert.equal(spec.exportMethod, "app-store-connect");
  assert.equal(spec.signing, "automatic");
  assert.equal(spec.buildNumber, "date");
  assert.equal(spec.checks.versionIdentity, true);
  assert.deepEqual(spec.prepare, []);
  assert.equal(spec.cleanup.derivedData, false);
  assert.equal(spec.teamId, TEAM);
  // The broker owns DerivedData and its lifetime: a recipe cannot clean it, and a request to is refused rather than silently ignored.
  invalid(baseSpec({ cleanup: { derivedData: true } }), /cleanup\.derivedData cannot be honoured: the broker owns/);
  assert.equal(validateIosPackageConfig(baseSpec({ cleanup: { derivedData: false } })).cleanup.derivedData, false);
});

test("unknown keys are rejected at every level", () => {
  invalid(baseSpec({ certificate: "x" }), /unknown key\(s\): certificate/);
  invalid(baseSpec({ cleanup: { everything: true } }), /cleanup has unknown key/);
  invalid(baseSpec({ checks: { signature: true } }), /checks has unknown key/);
  invalid(baseSpec({ prepare: [{ cmd: "node", shell: "sh" }] }), /prepare\[0\] has unknown key/);
  assert.throws(() => validateIosPackageConfig("nope"), IosPackageError);
});

test("required fields have no hard-coded default: a missing team id, bundle id, scheme, project, archive or ipa is refused", () => {
  for (const key of ["teamId", "bundleId", "scheme", "project", "archive", "ipa"]) {
    const spec = baseSpec(); delete spec[key];
    assert.throws(() => validateIosPackageConfig(spec), (error) => error.code === "IOS_PACKAGE_CONFIG_INVALID", key);
  }
  invalid(baseSpec({ teamId: "personal" }), /teamId/);
});

test("signing, export, build number and checks are restricted to what v1 supports", () => {
  invalid(baseSpec({ signing: "manual" }), /signing must be automatic/);
  invalid(baseSpec({ exportMethod: "ad-hoc" }), /exportMethod/);
  invalid(baseSpec({ buildNumber: "increment" }), /buildNumber/);
  invalid(baseSpec({ configuration: "Debug" }), /debug configuration/);
  invalid(baseSpec({ bundleId: "dev.orthic.pulse.ios.dev" }), /dev\/qa\/test\/debug/);
  invalid(baseSpec({ checks: { versionIdentity: false } }), /cannot be turned off/);
});

test("paths stay relative and inside the app; projectSpec sits beside the project", () => {
  invalid(baseSpec({ project: "/abs/Pulse.xcodeproj" }), /relative/);
  invalid(baseSpec({ archive: "../out/Pulse.xcarchive" }), /relative/);
  invalid(baseSpec({ ipa: "build/Pulse.zip" }), /\.ipa/);
  invalid(baseSpec({ projectSpec: "other/project.yml" }), /same directory/);
});

test("extraBuildSettings must be non-secret and cannot touch signing or the recipe's own settings", () => {
  assert.deepEqual(validateIosPackageConfig(baseSpec({ extraBuildSettings: { HR_MODEL_WORKER_BASE: "https://example.test" } })).extraBuildSettings, { HR_MODEL_WORKER_BASE: "https://example.test" });
  invalid(baseSpec({ extraBuildSettings: { HR_MODEL_AUTH_BEARER: "abc" } }), /looks like a secret/);
  invalid(baseSpec({ extraBuildSettings: { API_KEY: "abc" } }), /looks like a secret/);
  invalid(baseSpec({ extraBuildSettings: { CODE_SIGN_IDENTITY: "x" } }), /cannot be overridden/);
  invalid(baseSpec({ extraBuildSettings: { DEVELOPMENT_TEAM: "x" } }), /cannot be overridden/);
  invalid(baseSpec({ extraBuildSettings: { CURRENT_PROJECT_VERSION: "1" } }), /cannot be overridden/);
  invalid(baseSpec({ extraBuildSettings: { SOME_URL: "Bearer abcdefghijkl" } }), /non-secret/);
  invalid(baseSpec({ extraBuildSettings: { lower: "x" } }), /build setting name/);
});

// ---- pure builders ------------------------------------------------------------------------------------------------
test("ExportOptions.plist exports (never uploads), signs automatically and keeps the recipe's build number", () => {
  const xml = exportOptionsPlist(validateIosPackageConfig(baseSpec()));
  assert.match(xml, /<key>method<\/key><string>app-store-connect<\/string>/);
  assert.match(xml, /<key>destination<\/key><string>export<\/string>/);
  assert.match(xml, /<key>signingStyle<\/key><string>automatic<\/string>/);
  assert.match(xml, new RegExp(`<key>teamID</key><string>${TEAM}</string>`));
  assert.match(xml, /<key>manageAppVersionAndBuildNumber<\/key><false\/>/);
  assert.match(xml, /<key>stripSwiftSymbols<\/key><true\/>/);
});

test("the date build number is YYYYMMDDNN, rises within a day, restarts on a new day and stops at 99", () => {
  const dir = tempRoot("rk-ios-bn-");
  const stateFile = path.join(dir, "state", "ios-build-number.json");
  assert.equal(nextDateBuildNumber({ now: NOW, stateFile }), "2026101801", "a missing counter is the first build");
  assert.equal(nextDateBuildNumber({ now: NOW, stateFile }), "2026101802");
  assert.equal(nextDateBuildNumber({ now: new Date("2026-10-19T00:10:00Z"), stateFile }), "2026101901");
  writeFileSync(stateFile, JSON.stringify({ day: "20261019", seq: 99 }));
  assert.throws(() => nextDateBuildNumber({ now: new Date("2026-10-19T10:00:00Z"), stateFile }), (error) => error.code === "IOS_PACKAGE_BUILD_NUMBER" && /exhausted/.test(error.message));
});

test("the build number counter fails closed: corrupt, malformed, future or unreadable is an error, never a restart at 01", () => {
  const dir = tempRoot("rk-ios-bn-");
  const stateFile = path.join(dir, "ios-build-number.json");
  const stuck = (pattern) => assert.throws(() => nextDateBuildNumber({ now: NOW, stateFile }), (error) => error instanceof IosPackageError && error.code === "IOS_PACKAGE_BUILD_NUMBER" && pattern.test(error.message));
  writeFileSync(stateFile, "not json");
  stuck(/not valid JSON/);
  writeFileSync(stateFile, "");
  stuck(/not valid JSON/);
  for (const bad of [{ day: "2026-10-18", seq: 1 }, { day: "20261018", seq: 0 }, { day: "20261018", seq: 100 }, { day: "20261018", seq: "2" }, { seq: 3 }, null, []]) {
    writeFileSync(stateFile, JSON.stringify(bad));
    stuck(/malformed/);
  }
  writeFileSync(stateFile, JSON.stringify({ day: "20261019", seq: 4 }));
  stuck(/later than today/);
  assert.equal(readFileSync(stateFile, "utf8").includes("20261019"), true, "a refused counter is left untouched");
  // an unreadable counter (anything but ENOENT) is not a first build either
  const denied = { ...nodeFs, readFileSync: () => { throw Object.assign(new Error("denied"), { code: "EACCES" }); } };
  assert.throws(() => nextDateBuildNumber({ now: NOW, stateFile, fs: denied }), (error) => error.code === "IOS_PACKAGE_BUILD_NUMBER" && /unreadable \(EACCES\)/.test(error.message));
});

test("the counter is written atomically: a temporary file in the same directory, then a rename over the counter", () => {
  const dir = tempRoot("rk-ios-bn-");
  const stateFile = path.join(dir, "deep", "ios-build-number.json");
  const events = [];
  const spy = {
    ...nodeFs,
    mkdirSync: (...args) => { events.push("mkdir"); return nodeFs.mkdirSync(...args); },
    writeFileSync: (target, ...rest) => { events.push(`write ${path.basename(String(target))}`); return nodeFs.writeFileSync(target, ...rest); },
    renameSync: (from, to) => { events.push(`rename ${path.basename(String(from))} -> ${path.basename(String(to))}`); return nodeFs.renameSync(from, to); },
  };
  assert.equal(nextDateBuildNumber({ now: NOW, stateFile, fs: spy }), "2026101801");
  assert.equal(events[0], "mkdir");
  assert.match(events[1], /^write ios-build-number\.json\.tmp-\d+$/);
  assert.match(events[2], /^rename ios-build-number\.json\.tmp-\d+ -> ios-build-number\.json$/);
  assert.deepEqual(readdirSync(path.dirname(stateFile)), ["ios-build-number.json"], "no temporary file is left behind");
  assert.deepEqual(JSON.parse(readFileSync(stateFile, "utf8")), { day: "20261018", seq: 1 });
  // a crash between the write and the rename leaves the old counter intact
  nodeFs.writeFileSync(stateFile, JSON.stringify({ day: "20261018", seq: 5 }));
  const crashing = { ...nodeFs, renameSync: () => { throw new Error("power cut"); } };
  assert.throws(() => nextDateBuildNumber({ now: NOW, stateFile, fs: crashing }), /power cut/);
  assert.deepEqual(JSON.parse(readFileSync(stateFile, "utf8")), { day: "20261018", seq: 5 });
});

test("the archive command carries the configured team, automatic provisioning and the recipe's build number, no ASC credential, and names no output location (the broker owns them)", () => {
  const spec = validateIosPackageConfig(baseSpec({ extraBuildSettings: { HR_FLAG: "1" } }));
  const root = path.resolve("fixture-app");
  const args = archiveArgv(spec, { root, buildNumber: "2026101801" });
  assert.deepEqual(args.slice(0, 2), ["-project", path.join(root, "ios", "Pulse.xcodeproj")]);
  for (const expected of [`DEVELOPMENT_TEAM=${TEAM}`, "CURRENT_PROJECT_VERSION=2026101801", "HR_FLAG=1", "-allowProvisioningUpdates", "archive"]) assert.ok(args.includes(expected), expected);
  for (const owned of ["-derivedDataPath", "-archivePath", "-resultBundlePath", "-exportPath", "-clonedSourcePackagesDirPath", "-packageCachePath"]) assert.ok(!args.includes(owned), `${owned} is the broker's`);
  assert.equal(args[args.indexOf("-destination") + 1], "generic/platform=iOS");
  assert.equal(args.at(-1), "archive");
  assert.ok(!args.some((arg) => /authenticationKey|AuthKey|\.p8/i.test(arg)));
  assert.deepEqual(xcodegenArgv(spec, root), ["generate", "--spec", path.join(root, "ios", "project.yml"), "--project", path.join(root, "ios")]);
  const exported = exportArgv(spec, { optionsFile: "/x/o.plist" });
  assert.deepEqual(exported, ["-exportArchive", "-exportOptionsPlist", "/x/o.plist", "-allowProvisioningUpdates"]);
  assert.ok(!exported.includes("-archivePath") && !exported.includes("-exportPath"), "the export reads the managed archive and writes the managed export directory");
});

test("the argv the recipe sends is exactly what the real native door and the broker's grammar admit (xcodegen, archive, export), with the managed roles the receipt will name", () => {
  const root = tempRoot("rk-ios-door-");
  mkdirSync(path.join(root, ".right-release"), { recursive: true });
  const spec = validateIosPackageConfig(baseSpec({ extraBuildSettings: { HR_FLAG: "1" } }));
  const optionsFile = path.join(root, IOS_EXPORT_OPTIONS_RELATIVE);
  writeFileSync(optionsFile, exportOptionsPlist(spec));
  const runner = realRunner(root);
  const generate = runner.plan("xcodegen", xcodegenArgv(spec, root), { cwd: root });
  assert.deepEqual(generate.native.args, ["generate", "--spec", "ios/project.yml", "--project", "ios"]);
  const archive = runner.plan("xcodebuild", archiveArgv(spec, { root, buildNumber: "2026101801" }), { cwd: root });
  assert.deepEqual([archive.command, archive.args.slice(0, 3)], [MAC_CLIENT, ["run", "rightkit-xcodebuild", "--"]]);
  assert.equal(archive.native.args[archive.native.args.indexOf("-project") + 1], "ios/Pulse.xcodeproj", "paths are made relative to the step's directory");
  const exported = runner.plan("xcodebuild", exportArgv(spec, { optionsFile }), { cwd: root });
  assert.equal(exported.native.args[exported.native.args.indexOf("-exportOptionsPlist") + 1], ".right-release/ios-export-options.plist");
  const state = nativeStateDir(path.join(root, "out"), "xcodebuild", "");
  assert.deepEqual(nativeManagedOutputs("xcodebuild", archive.native.args, { state }).outputs.map((entry) => entry.role), ["derived-data", "archive"]);
  assert.deepEqual(nativeManagedOutputs("xcodebuild", exported.native.args, { state }).outputs.map((entry) => entry.role), ["archive", "export"]);
  // The old spellings the broker owns are refused by the real door, never sent.
  for (const owned of [["-archivePath", "/x/A.xcarchive"], ["-derivedDataPath", "/x/dd"], ["-resultBundlePath", "/x/r.xcresult"]]) {
    assert.throws(() => runner.plan("xcodebuild", [...archiveArgv(spec, { root, buildNumber: "2026101801" }), ...owned], { cwd: root }), (error) => error.code === "RK_BUILD_LAUNCH_REFUSED" && error.reason === "RK_NATIVE_TOOL_REFUSED", owned[0]);
  }
  assert.throws(() => runner.plan("xcodebuild", ["-exportArchive", "-exportPath", "/x/e", "-exportOptionsPlist", optionsFile], { cwd: root }), (error) => error.reason === "RK_NATIVE_TOOL_REFUSED");
});

test("version identity: one version across app, appex and Watch, no unresolved $(...), the recipe's build number", () => {
  const info = (role, version, build) => ({ role, label: role, info: { CFBundleShortVersionString: version, CFBundleVersion: build } });
  assert.deepEqual(assertVersionIdentity([info("app", "1.2.0", "2026101801"), info("appex", "1.2.0", "2026101801"), info("watch", "1.2.0", "2026101801")], { expectedBuild: "2026101801" }), { marketingVersion: "1.2.0", buildNumber: "2026101801" });
  assert.throws(() => assertVersionIdentity([info("app", "1.2.0", "7"), info("watch", "1.2.1", "7")]), /version drift: watch/);
  assert.throws(() => assertVersionIdentity([info("app", "1.2.0", "7"), info("appex", "1.2.0", "8")]), /version drift: appex/);
  assert.throws(() => assertVersionIdentity([info("app", "$(MARKETING_VERSION)", "7")]), /unresolved/);
  assert.throws(() => assertVersionIdentity([info("app", "1.2.0", "0.1.0")], { expectedBuild: "2026101801" }), /not the recipe's/);
});

test("distribution entitlements and profile checks", () => {
  assert.doesNotThrow(() => assertDistributionEntitlements({ "aps-environment": "production", "application-identifier": `${TEAM}.${BUNDLE}` }, "app"));
  assert.doesNotThrow(() => assertDistributionEntitlements({}, "appex"));
  assert.throws(() => assertDistributionEntitlements({ "get-task-allow": true }, "app"), /get-task-allow/);
  assert.throws(() => assertDistributionEntitlements({ "aps-environment": "development" }, "app"), /production/);
  assert.throws(() => assertDistributionEntitlements(null, "app"), /no readable entitlements/);
  const profile = { TeamIdentifier: [TEAM], Entitlements: { "application-identifier": `${TEAM}.${BUNDLE}`, "get-task-allow": false }, Name: "iOS Team Store", UUID: "u-1", ExpirationDate: "2027-01-01T00:00:00Z" };
  assert.equal(assertDistributionProfile(profile, { teamId: TEAM, bundleId: BUNDLE }).applicationIdentifier, `${TEAM}.${BUNDLE}`);
  assert.throws(() => assertDistributionProfile({ ...profile, ProvisionedDevices: ["x"] }, { teamId: TEAM, bundleId: BUNDLE }), /development, ad hoc or enterprise/);
  assert.throws(() => assertDistributionProfile({ ...profile, TeamIdentifier: ["OTHERTEAM1"] }, { teamId: TEAM, bundleId: BUNDLE }), /not for team/);
  assert.throws(() => assertDistributionProfile(profile, { teamId: TEAM, bundleId: "dev.other.app" }), /authorizes/);
});

// CodeRight's real pins (the five packages whose macros Xcode refuses until they are trusted).
const CODERIGHT_PINS = {
  "swift-composable-architecture": "ead11e04e5011c437722c1990d22f80d87056978",
  "swift-case-paths": "1866d1046fd4e54d590af6be5b23de6ca3af2bf9",
  "swift-dependencies": "5e0815746534ccaa9e20588ea35d3bb2cb27bab8",
  "swift-perception": "de219a1cf34e958134e75a9ebb134cf09bf52fc6",
  "swift-navigation": "418a57db2e757b02817dfd3d8a0374d5794664ab",
};
const trustList = (pins = CODERIGHT_PINS) => Object.entries(pins).map(([name, revision]) => ({ package: name, revision }));
const resolvedFile = (pins, version = 3) => JSON.stringify({
  originHash: "x", version,
  pins: Object.entries(pins).map(([identity, revision]) => ({ identity, kind: "remoteSourceControl", location: `https://github.com/pointfreeco/${identity}`, state: { revision, version: "1.0.0" } })),
});
const trustSpec = (extra = {}) => ({ packageResolved: "Package.resolved", trustedSwiftMacros: trustList(), ...extra });
const SKIP_FLAGS = ["-skipMacroValidation", "-skipPackagePluginValidation"];
const archiveCall = (f) => f.calls.find((call) => call.cmd === "xcodebuild" && call.args.includes("archive"));

// ---- trusted Swift macros / plugins: config -----------------------------------------------------------------------
test("trustedSwiftMacros config is validated strictly: 40 hex revisions, no duplicates, no unknown keys, no stray packageResolved", () => {
  const ok = validateIosPackageConfig(baseSpec(trustSpec()));
  assert.equal(ok.trustedSwiftMacros.length, 5);
  assert.deepEqual(ok.trustedSwiftPlugins, []);
  const rev = CODERIGHT_PINS["swift-case-paths"];
  const entry = (extra = {}) => ({ package: "swift-case-paths", revision: rev, ...extra });
  invalid(baseSpec(trustSpec({ trustedSwiftMacros: "swift-case-paths" })), /trustedSwiftMacros must be an array/);
  invalid(baseSpec(trustSpec({ trustedSwiftMacros: ["swift-case-paths"] })), /trustedSwiftMacros\[0\] must be an object/);
  invalid(baseSpec(trustSpec({ trustedSwiftMacros: [entry({ version: "1.0.0" })] })), /unknown key\(s\): version/);
  invalid(baseSpec(trustSpec({ trustedSwiftMacros: [entry({ revision: "1866d10" })] })), /exact 40 character/);
  invalid(baseSpec(trustSpec({ trustedSwiftMacros: [entry({ revision: rev.toUpperCase() })] })), /exact 40 character/);
  invalid(baseSpec(trustSpec({ trustedSwiftMacros: [entry({ revision: `${rev}0` })] })), /exact 40 character/);
  invalid(baseSpec(trustSpec({ trustedSwiftMacros: [entry({ revision: "1.2.3" })] })), /exact 40 character/);
  invalid(baseSpec(trustSpec({ trustedSwiftMacros: [entry({ package: "Swift-Case-Paths" })] })), /lowercase SwiftPM package identity/);
  invalid(baseSpec(trustSpec({ trustedSwiftMacros: [entry({ package: "https://github.com/pointfreeco/swift-case-paths" })] })), /package identity/);
  invalid(baseSpec(trustSpec({ trustedSwiftMacros: [entry(), entry()] })), /lists swift-case-paths twice/);
  invalid(baseSpec(trustSpec({ trustedSwiftPlugins: [entry(), entry()] })), /trustedSwiftPlugins lists swift-case-paths twice/);
  invalid(baseSpec({ trustedSwiftMacros: [entry()] }), /need packageResolved/); // xcodegen project: its own Package.resolved is untracked
  invalid(baseSpec({ packageResolved: "Package.resolved" }), /only used with trustedSwift/);
  invalid(baseSpec(trustSpec({ packageResolved: "../Package.resolved" })), /relative/);
  invalid(baseSpec(trustSpec({ packageResolved: "Other.json" })), /must match .*Package/);
  // No projectSpec: the in-xcodeproj default is allowed without packageResolved.
  assert.equal(validateIosPackageConfig(baseSpec({ projectSpec: undefined, trustedSwiftMacros: [entry()] })).packageResolved, null);
});

test("no trust list: the archive has no skip or resolution flag, the plan has no trust step and the receipt has no swiftTrust (Pulse unchanged)", async () => {
  const spec = validateIosPackageConfig(baseSpec());
  assert.deepEqual([spec.packageResolved, spec.trustedSwiftMacros, spec.trustedSwiftPlugins], [null, [], []]);
  const args = archiveArgv(spec, { root: path.resolve("fixture-app"), buildNumber: "2026101801" });
  for (const flag of [...SKIP_FLAGS, "-disableAutomaticPackageResolution", "-onlyUsePackageVersionsFromResolvedFile"]) assert.ok(!args.includes(flag), flag);
  assert.ok(!iosPackagePlan(spec, { root: "/app", receiptDir: "/r" }).steps.some((step) => step.id === "trust-check"));
  const f = fixture();
  await runIosPackageRecipe(f.deps);
  assert.ok(!archiveCall(f).args.some((arg) => SKIP_FLAGS.includes(arg) || /Package(Resolution|Versions)/.test(arg)));
  assert.ok(!f.calls.some((call) => call.cmd === "git"), "no git call without a trust list");
  assert.equal(JSON.parse(readFileSync(f.receipt, "utf8")).swiftTrust, undefined);
});

// ---- trusted Swift macros / plugins: pins ------------------------------------------------------------------------
test("Package.resolved versions 2 and 3 parse; other versions, bad JSON and duplicate identities are refused", () => {
  for (const version of [2, 3]) {
    const pins = parsePackageResolved(resolvedFile(CODERIGHT_PINS, version));
    assert.equal(pins.get("swift-perception").revision, CODERIGHT_PINS["swift-perception"]);
  }
  const code = (error) => error instanceof IosPackageError && error.code === "IOS_PACKAGE_TRUST_FAILED";
  assert.throws(() => parsePackageResolved(JSON.stringify({ version: 1, object: { pins: [] } })), code);
  assert.throws(() => parsePackageResolved("{"), code);
  assert.throws(() => parsePackageResolved(JSON.stringify({ version: 3 })), code);
  assert.throws(() => parsePackageResolved(JSON.stringify({ version: 3, pins: [{ identity: "a", state: {} }, { identity: "A", state: {} }] })), /twice/);
});

test("assertTrustedPins names every mismatching package; skip flags need its verified evidence", () => {
  const pins = parsePackageResolved(resolvedFile({ ...CODERIGHT_PINS, "swift-case-paths": "f".repeat(40) }));
  const spec = validateIosPackageConfig(baseSpec(trustSpec({ trustedSwiftPlugins: [{ package: "swiftlint-plugin", revision: "1".repeat(40) }] })));
  assert.throws(() => assertTrustedPins(pins, spec), (error) => error.code === "IOS_PACKAGE_TRUST_MISMATCH"
    && error.message.includes(`swift-case-paths is pinned to ${"f".repeat(40)}`) && error.message.includes(`reviewed revision is ${CODERIGHT_PINS["swift-case-paths"]}`)
    && error.message.includes("trustedSwiftPlugins: swiftlint-plugin is not pinned"));
  const noRevision = parsePackageResolved(JSON.stringify({ version: 3, pins: [{ identity: "swift-case-paths", kind: "localSourceControl", location: "/x", state: { version: "1.0.0" } }] }));
  assert.throws(() => assertTrustedPins(noRevision, { trustedSwiftMacros: [{ package: "swift-case-paths", revision: CODERIGHT_PINS["swift-case-paths"] }] }), /pinned without a git revision/);
  // Fail closed: a list without verified evidence (absent, or a forged object) never yields a skip flag.
  assert.throws(() => swiftTrustFlags(spec), (error) => error.code === "IOS_PACKAGE_TRUST_FAILED");
  assert.throws(() => swiftTrustFlags(spec, { macros: [], plugins: [] }), (error) => error.code === "IOS_PACKAGE_TRUST_FAILED");
  assert.throws(() => archiveArgv(spec, { root: "/app", buildNumber: "1" }), /not verified/);
  const good = assertTrustedPins(parsePackageResolved(resolvedFile({ ...CODERIGHT_PINS, "swiftlint-plugin": "1".repeat(40) })), spec);
  assert.deepEqual(swiftTrustFlags(spec, good), ["-skipMacroValidation", "-skipPackagePluginValidation", "-disableAutomaticPackageResolution", "-onlyUsePackageVersionsFromResolvedFile"]);
});

// ---- the run, with every dependency injected ----------------------------------------------------------------------
// The native runner. `planner` is the REAL production runner's planner (build-launch.mjs createNativeRunner on a Mac host description): every native call the recipe makes is judged by the real
// door, the real adapter grammar and the real capability check, and a refusal is the real runner's own typed result (it launches nothing). Only the broker's EXECUTION is stood in for (`execute`): the
// output roles the stand-in hands back are computed by the real grammar (`nativeManagedOutputs`) under a job output directory, exactly the mapping a carrier reads back from a signed receipt.
// It is a unit seam for the recipe's own logic, never evidence that a Mac fixture archives or exports: that is root's (a real Xcode, the installed broker).
const MAC_ENV = { PATH: "/usr/bin", HOME: "/Users/rk" };
const MAC_CLIENT = managedRuntimeClient(MAC_ENV, "darwin", () => true);
const realRunner = (cwd, brokerCapabilities) => createNativeRunner({
  cwd, env: MAC_ENV, platform: "darwin", exists: (p) => p === MAC_CLIENT, which: (c) => (c === "rustright" ? ["/elsewhere/on/path/rustright"] : []), readFile: () => "",
  ...(brokerCapabilities ? { brokerCapabilities } : {}),
});
function brokerStandIn({ cwd, execute, brokerCapabilities, enforceGrammar = true, outputDir = path.join(cwd, "broker-output"), outputsOf }) {
  const real = realRunner(cwd, brokerCapabilities);
  const launched = [];
  let jobs = 0;
  // Only for the trust-flow tests (see `enforceGrammar`): the door's relativization of absolute paths is kept, its grammar is not.
  const permissive = (command, args) => ({ native: { tool: nativeToolOf(command), args: args.map(String).map((word, index, all) => (all[index - 1] === "-exportOptionsPlist" && path.isAbsolute(word) ? path.relative(cwd, word) : word)) } });
  // The stand-in is its own host description (MAC_ENV): the recipe's `env` is the step's launch environment, which the runner never forwards.
  const hostOptions = (options) => ({ ...options, cwd: options.cwd ?? cwd, env: MAC_ENV });
  const planner = enforceGrammar ? (command, args, options = {}) => real.plan(command, args, hostOptions(options)) : permissive;
  return {
    launched,
    plan: planner,
    run(command, args = [], options = {}) {
      let launch;
      try { launch = planner(command, args, options); } catch { return real.run(command, args, hostOptions(options)); }
      jobs += 1;
      const jobId = `job-${jobs}`;
      const { tool, args: sent } = launch.native;
      const state = nativeStateDir(outputDir, tool, "");
      const mapped = nativeManagedOutputs(tool, sent, { state, jobId });
      const byRole = Object.fromEntries((mapped.outputs ?? []).map((entry) => [entry.role, entry.path]));
      launched.push({ command, args: [...sent] });
      const raw = execute({ command, args: [...sent], options, byRole, jobId });
      const ok = raw.status === 0 && !raw.error;
      const outputs = ok ? (outputsOf ? outputsOf({ tool, byRole, mapped, state }) : { ok: true, source: "receipt", state, outputs: mapped.outputs, byRole }) : null;
      return { ok, status: raw.status ?? null, signal: null, stdout: raw.stdout ?? "", stderr: raw.stderr ?? "", error: raw.error ?? null, refusal: null, tool, adapter: NATIVE_ADAPTERS[tool].adapter, capability: NATIVE_ADAPTERS[tool].capability, argv: [...sent], carried: true, relativeCwd: "", requestId: jobId, outputDir, outputs, nestedChildren: null };
    },
  };
}

function fixture(over = {}) {
  const root = tempRoot("rk-ios-recipe-");
  mkdirSync(path.join(root, "ios"), { recursive: true });
  writeFileSync(path.join(root, "ios", "project.yml"), "name: Pulse\n");
  const spec = validateIosPackageConfig(baseSpec(over.spec));
  const calls = [];
  const scans = [];
  let head = "a".repeat(40);
  const bundleInfo = (build, extra = {}) => ({ CFBundleIdentifier: BUNDLE, CFBundleShortVersionString: "0.1.0", CFBundleVersion: build, ITSAppUsesNonExemptEncryption: false, ...extra });
  const writeBundle = (dir, info, binary = "fake-macho") => { mkdirSync(dir, { recursive: true }); writeFileSync(path.join(dir, "Info.plist"), JSON.stringify(info)); writeFileSync(path.join(dir, "exe"), binary); };
  const ok = (stdout = "") => ({ status: 0, stdout, stderr: "" });
  let lastArchive;
  const failing = (cmd, args) => over.fail === `${cmd} ${args[0]}` || (over.fail === "xcodebuild archive" && cmd === "xcodebuild" && args.includes("archive"));
  // The broker's side: what a managed xcodegen / xcodebuild job does. Output goes only where the grammar's managed roles say.
  const execute = ({ command, args, byRole }) => {
    calls.push({ cmd: command, args: [...args], via: "native" });
    if (failing(command, args)) return { status: 1, stdout: "", stderr: "boom: sensitive Bearer abcdefghijklmnop" };
    if (command === "xcodegen" && args[0] === "generate") { mkdirSync(path.join(root, "ios", "Pulse.xcodeproj"), { recursive: true }); return ok(); }
    if (command === "xcodebuild" && args.includes("archive")) {
      const archive = byRole.archive;
      const build = args.find((arg) => arg.startsWith("CURRENT_PROJECT_VERSION=")).split("=")[1];
      const app = path.join(archive, "Products", "Applications", "Pulse.app");
      writeBundle(app, bundleInfo(over.archiveBuild ?? build, over.archiveInfo), over.binary);
      writeBundle(path.join(app, "PlugIns", "Share.appex"), bundleInfo(build, { CFBundleIdentifier: `${BUNDLE}.share`, ...(over.appexInfo ?? {}) }));
      writeBundle(path.join(app, "Watch", "Pulse Watch.app"), bundleInfo(build, { CFBundleIdentifier: `${BUNDLE}.watchkitapp` }));
      lastArchive = archive;
      return ok("** ARCHIVE SUCCEEDED **");
    }
    if (command === "xcodebuild" && args[0] === "-exportArchive") {
      mkdirSync(byRole.export, { recursive: true });
      writeFileSync(path.join(byRole.export, over.ipaName ?? "Pulse.ipa"), `IPA-BYTES-${calls.length}`);
      if (over.extraIpa) writeFileSync(path.join(byRole.export, "Second.ipa"), "x");
      return ok("** EXPORT SUCCEEDED **");
    }
    return { status: 1, stdout: "", stderr: `unexpected native call ${command} ${args.join(" ")}` };
  };
  const standIn = brokerStandIn({
    cwd: root, execute, brokerCapabilities: over.brokerCapabilities,
    // The reviewed-trust flags (-skipMacroValidation, ...) are not in the broker's registered xcodebuild grammar today: the trust-flow tests exercise the recipe's own logic with a permissive planner, and the
    // dedicated refusal test below pins the real door's verdict.
    enforceGrammar: over.enforceGrammar ?? !(over.spec?.trustedSwiftMacros || over.spec?.trustedSwiftPlugins),
    outputsOf: over.outputsOf ? (info) => over.outputsOf(info) : undefined,
  });
  // The direct, non-compiling steps (queries, plutil, codesign, security, ditto, git and the app's own non-native prepare commands).
  const run = (cmd, args, options = {}) => {
    calls.push({ cmd, args: [...args], cwd: options.cwd, input: options.input, via: "direct" });
    const id = `${cmd} ${args[0]}`;
    if (over.fail === id) return { status: 1, stdout: "", stderr: "boom: sensitive Bearer abcdefghijklmnop" };
    if (cmd === "xcodebuild" && args[0] === "-version") return ok("Xcode 26.0\nBuild version 17A324\n");
    if (cmd === "xcodegen" && args[0] === "--version") return over.noXcodegen ? { status: 127, error: new Error("spawn xcodegen ENOENT") } : ok("2.43.0");
    if (cmd === "node") return ok();
    if (cmd === "ditto") {
      const target = args.at(-1);
      cpSync(path.join(lastArchive, "Products", "Applications", "Pulse.app"), path.join(target, "Payload", "Pulse.app"), { recursive: true });
      const app = path.join(target, "Payload", "Pulse.app");
      for (const dir of [app, path.join(app, "PlugIns", "Share.appex"), path.join(app, "Watch", "Pulse Watch.app")]) {
        if (over.noAppexProfile && dir.endsWith("Share.appex")) continue;
        const id = JSON.parse(readFileSync(path.join(dir, "Info.plist"), "utf8")).CFBundleIdentifier;
        writeFileSync(path.join(dir, "embedded.mobileprovision"), JSON.stringify({ bundle: over.profileBundle?.[path.basename(dir)] ?? id }));
      }
      over.mutateIpa?.(app);
      return ok();
    }
    if (cmd === "plutil") {
      if (options.input !== undefined) return ok(String(options.input).replace(/^<plist-json>/, ""));
      try { return ok(readFileSync(args.at(-1), "utf8")); } catch { return { status: 1, stdout: "", stderr: "" }; }
    }
    if (cmd === "codesign" && args[0] === "--verify") return over.badSignature ? { status: 1, stdout: "", stderr: "invalid signature" } : ok();
    if (cmd === "codesign" && args[0] === "-d" && over.emptyAppexEntitlements && args.at(-1).endsWith("Share.appex")) return ok("");
    if (cmd === "codesign" && args[0] === "-d") return ok(`<plist-json>${JSON.stringify(over.entitlements ?? { "application-identifier": `${TEAM}.${BUNDLE}`, "aps-environment": "production", "get-task-allow": false })}`);
    if (cmd === "security" && args[0] === "cms") {
      const { bundle } = JSON.parse(readFileSync(args.at(-1), "utf8"));
      return ok(`<plist-json>${JSON.stringify(over.profile ?? { TeamIdentifier: [TEAM], Entitlements: { "application-identifier": `${TEAM}.${bundle}`, "get-task-allow": false }, Name: "iOS Team Store", UUID: "uuid-1", ExpirationDate: "2027-01-01T00:00:00Z" })}`);
    }
    if (cmd === "git" && args[0] === "ls-files") return over.untrackedPins ? { status: 1, stdout: "", stderr: "error: pathspec did not match" } : ok(args.at(-1));
    if (cmd === "git" && args[0] === "diff") return over.dirtyPins ? { status: 1, stdout: "", stderr: "" } : ok();
    // A compiler-capable tool reaching the direct runner is the bug under test: it must never happen.
    if (nativeToolOf(cmd) !== null) return { status: 1, stdout: "", stderr: `a native build tool reached the direct runner: ${cmd} ${args.join(" ")}` };
    return { status: 1, stdout: "", stderr: `unexpected command ${cmd} ${args.join(" ")}` };
  };
  if (over.pins !== undefined) {
    const pinsFile = path.join(root, over.pinsPath ?? "Package.resolved");
    mkdirSync(path.dirname(pinsFile), { recursive: true });
    writeFileSync(pinsFile, typeof over.pins === "string" ? over.pins : resolvedFile(over.pins));
  }
  const deps = {
    root, spec, env: { PATH: "/usr/bin" }, platform: "darwin", run, native: standIn, now: () => NOW, log: () => {}, tmpRoot: root, counterDir: path.join(root, "vault-state", "ios-build-numbers"),
    detectSession: () => ({ guiRunner: false, reason: "Aqua security session" }),
    gitHead: () => head,
    scanFiles: (label, files, scanOptions) => { scans.push({ label, files, scanOptions }); over.onScan?.(label); },
    ...over.deps,
  };
  const managedState = path.join(root, "broker-output", "native", "xcodebuild", createHash("sha256").update("").digest("hex"));
  return { root, spec, calls, scans, deps, standIn, setHead: (value) => { head = value; }, ipa: path.join(root, "build", "ios", "export", "Pulse.ipa"), managedArchive: path.join(managedState, "archive", "App.xcarchive"), managedExport: path.join(managedState, "export"), optionsFile: path.join(root, ".right-release", "ios-export-options.plist"), receipt: path.join(root, ".right-release", "receipts", IOS_RECEIPT_NAME) };
}
const rejects = (promise, code, pattern) => assert.rejects(promise, (error) => error instanceof IosPackageError && error.code === code && (!pattern || pattern.test(error.message)), `${code} ${pattern ?? ""}`);
const sha = (bytes) => createHash("sha256").update(bytes).digest("hex");

test("dry run prints the ordered plan on any host and changes nothing", async () => {
  const f = fixture({ spec: { prepare: [{ cmd: "node", args: ["scripts/prep.mjs"] }] } });
  const lines = [];
  const result = await runIosPackageRecipe({ ...f.deps, platform: "win32", dryRun: true, run: () => assert.fail("a dry run runs nothing"), log: (line) => lines.push(line) });
  assert.equal(result.dryRun, true);
  assert.deepEqual(result.plan.steps.map((step) => step.id), ["preflight", "generate", "prepare", "archive", "verify-archive", "export", "verify-ipa", "receipt"]);
  assert.deepEqual(IOS_PACKAGE_STEPS, result.plan.steps.map((step) => step.id));
  assert.ok(lines.some((line) => /dry-run/.test(line)));
  assert.ok(!existsSync(path.join(f.root, ".right-release")), "no receipt or state directory");
  assert.ok(!existsSync(f.archive));
  const noGen = iosPackagePlan(validateIosPackageConfig(baseSpec({ projectSpec: undefined })), { root: f.root, receiptDir: "/r" });
  assert.ok(!noGen.steps.some((step) => step.id === "generate"));
});

test("a non-macOS host is refused before any command or file", async () => {
  for (const platform of ["win32", "linux"]) {
    const f = fixture();
    await rejects(runIosPackageRecipe({ ...f.deps, platform }), "IOS_PACKAGE_HOST_UNSUPPORTED", /needs macOS/);
    assert.equal(f.calls.length, 0);
    assert.ok(!existsSync(path.join(f.root, ".right-release")));
  }
});

test("an SSH or other non-Aqua session is refused unless a GUI route is supplied, and then the route runs the Xcode commands", async () => {
  const f = fixture({ deps: { detectSession: () => ({ guiRunner: true, reason: "SSH security session" }) } });
  await rejects(runIosPackageRecipe(f.deps), "IOS_PACKAGE_HOST_UNSUPPORTED", /GUI \(Aqua\) session/);
  assert.equal(f.calls.length, 0);
  const routed = [];
  const g = fixture({ deps: { detectSession: () => ({ guiRunner: true, reason: "SSH security session" }) } });
  const viaRun = g.deps.run;
  await runIosPackageRecipe({ ...g.deps, route: (cmd, args, options) => { routed.push(cmd); return viaRun(cmd, args, options); }, run: () => assert.fail("the GUI route must run the Xcode commands") });
  assert.ok(routed.includes("xcodebuild"));
});

test("the full run executes the steps in order and writes a receipt over the final IPA bytes", async () => {
  const f = fixture({ spec: { prepare: [{ cmd: "node", args: ["scripts/prep.mjs"], cwd: "ios" }] } });
  const result = await runIosPackageRecipe(f.deps);
  const order = f.calls.map((call) => `${call.cmd}:${call.args[0]}`);
  assert.deepEqual(order.slice(0, 6), ["xcodebuild:-version", "xcodegen:--version", "xcodegen:generate", "node:scripts/prep.mjs", "xcodebuild:-project", "plutil:-convert"]);
  // Compiler-capable calls are native adapter jobs; only the queries and the app's own non-native prepare step ran directly.
  assert.deepEqual(f.calls.filter((call) => call.via === "native").map((call) => `${call.cmd}:${call.args.includes("archive") ? "archive" : call.args[0]}`), ["xcodegen:generate", "xcodebuild:archive", "xcodebuild:-exportArchive"]);
  assert.deepEqual(f.calls.filter((call) => call.via === "direct" && ["xcodegen", "xcodebuild"].includes(call.cmd)).map((call) => call.args[0]), ["-version", "--version"]);
  assert.ok(order.indexOf("xcodebuild:-exportArchive") > order.indexOf("xcodebuild:-project"));
  assert.ok(order.indexOf("ditto:-x") > order.indexOf("xcodebuild:-exportArchive"));
  assert.equal(path.resolve(f.calls.find((call) => call.cmd === "node").cwd), path.join(f.root, "ios"));
  assert.ok(existsSync(f.ipa) && existsSync(f.receipt));
  const receipt = JSON.parse(readFileSync(f.receipt, "utf8"));
  assert.equal(receipt.schema, 1);
  assert.equal(receipt.contract, IOS_SIGNING_CONTRACT);
  assert.equal(receipt.file.sha256, sha(readFileSync(f.ipa)));
  assert.equal(receipt.file.sizeBytes, readFileSync(f.ipa).length);
  assert.equal(receipt.file.name, "Pulse.ipa");
  assert.equal(receipt.teamId, TEAM);
  assert.equal(receipt.bundleId, BUNDLE);
  assert.equal(receipt.marketingVersion, "0.1.0");
  assert.equal(receipt.buildNumber, "2026101801");
  assert.equal(receipt.sourceCommit, "a".repeat(40));
  assert.match(receipt.xcodebuild, /Xcode 26\.0/);
  assert.equal(receipt.profile.applicationIdentifier, `${TEAM}.${BUNDLE}`);
  assert.deepEqual(receipt.bundles.map((bundle) => bundle.role), ["app", "appex", "watch"]);
  assert.deepEqual(result.sealEntry, { role: "ipa", name: "Pulse.ipa", sha256: receipt.file.sha256, sizeBytes: receipt.file.sizeBytes });
  assert.equal(verifyIosReceipt(f.receipt, f.ipa).file.sha256, receipt.file.sha256);
  assert.ok(existsSync(f.optionsFile));
  assert.equal(receipt.exportOptionsSha256, sha(readFileSync(f.optionsFile)));
  // RA-09: the broker's output readback is in the receipt: the archive and the export are the managed roles, never paths the recipe chose.
  assert.equal(receipt.native.archivePath, f.managedArchive);
  assert.deepEqual(receipt.native.archive.outputs.map((entry) => entry.role), ["derived-data", "archive"]);
  assert.deepEqual(receipt.native.export.outputs, [{ role: "archive", path: f.managedArchive }, { role: "export", path: f.managedExport }]);
  assert.equal(receipt.native.archive.adapter, "rightkit-xcodebuild");
  assert.equal(receipt.native.archive.capability, "adapter:rightkit-xcodebuild");
  assert.equal(receipt.native.archive.outputsSource, "receipt");
  assert.match(receipt.native.export.requestId, /^job-\d+$/);
  assert.equal(readFileSync(f.ipa, "utf8"), readFileSync(path.join(f.managedExport, "Pulse.ipa"), "utf8"), "the IPA is a verified copy of the managed export");
  assert.ok(existsSync(path.join(f.managedExport, "Pulse.ipa")), "the broker's managed export is left to the broker");
});

test("archive and export run with automatic provisioning, the configured team and no App Store Connect credential, and never name an output location", async () => {
  const f = fixture({ spec: { extraBuildSettings: { HR_FLAG: "1" } } });
  await runIosPackageRecipe({ ...f.deps, env: { PATH: "/usr/bin", APPLE_API_KEY_PATH: "/Users/me/AuthKey_X.p8" } });
  const archive = f.calls.find((call) => call.cmd === "xcodebuild" && call.args.includes("archive")).args;
  const exported = f.calls.find((call) => call.args[0] === "-exportArchive").args;
  for (const args of [archive, exported]) {
    assert.ok(args.includes("-allowProvisioningUpdates"));
    assert.ok(!args.some((arg) => /authenticationKey|AuthKey|\.p8/i.test(arg)));
    for (const owned of ["-derivedDataPath", "-archivePath", "-exportPath", "-resultBundlePath"]) assert.ok(!args.includes(owned), owned);
  }
  assert.ok(archive.includes(`DEVELOPMENT_TEAM=${TEAM}`) && archive.includes("CURRENT_PROJECT_VERSION=2026101801") && archive.includes("HR_FLAG=1"));
  assert.deepEqual(exported, ["-exportArchive", "-exportOptionsPlist", slashed(IOS_EXPORT_OPTIONS_RELATIVE), "-allowProvisioningUpdates"]);
});

test("the ExportOptions.plist is a regular XML file inside the step directory (the snapshot), never under the receipt directory", async () => {
  const f = fixture();
  const receiptDir = path.join(f.root, "elsewhere", "receipts");
  await runIosPackageRecipe({ ...f.deps, receiptDir });
  assert.ok(existsSync(f.optionsFile) && nodeFs.lstatSync(f.optionsFile).isFile());
  assert.ok(readFileSync(f.optionsFile, "utf8").startsWith("<?xml"));
  assert.ok(!existsSync(path.join(receiptDir, "ios-export-options.plist")), "nothing the broker reads lives under the receipt directory");
  assert.equal(path.relative(f.root, f.optionsFile).startsWith(".."), false);
});

test("no native runner, no spawn: a missing runner is refused before any command, and a compiler-capable tool never reaches the direct runner", async () => {
  const f = fixture();
  await rejects(runIosPackageRecipe({ ...f.deps, native: undefined }), "IOS_PACKAGE_NATIVE_REQUIRED", /createNativeRunner/);
  await rejects(runIosPackageRecipe({ ...f.deps, native: { run: () => ({}) } }), "IOS_PACKAGE_NATIVE_REQUIRED");
  assert.equal(f.calls.length, 0);
  assert.ok(!existsSync(path.join(f.root, ".right-release")));
  // A full run leaves the direct runner with the non-compiling steps only (the fixture's direct runner fails any xcodegen/xcodebuild call that is not a version query).
  await runIosPackageRecipe(f.deps);
  assert.ok(f.calls.filter((call) => call.via === "direct" && ["xcodegen", "xcodebuild"].includes(call.cmd)).every((call) => ["-version", "--version"].includes(call.args[0])));
});

test("a missing broker capability refuses BEFORE the first launch: nothing generated, archived or exported, and the typed capability is named", async () => {
  const f = fixture({ brokerCapabilities: ["adapter:rightkit-xcodegen"] });
  await rejects(runIosPackageRecipe(f.deps), "IOS_PACKAGE_NATIVE_REFUSED", /RK_NATIVE_CAPABILITY_MISSING[^]*adapter:rightkit-xcodebuild/);
  assert.equal(f.standIn.launched.length, 0, "not even xcodegen was launched");
  assert.ok(!f.calls.some((call) => call.via === "native"));
  assert.ok(!existsSync(f.ipa) && !existsSync(f.receipt));
  assert.ok(!existsSync(path.join(f.root, "ios", "Pulse.xcodeproj")));
});

test("an argv the real grammar refuses is refused before any launch with no generic spawn to fall back to (a trust list whose skip flags the broker does not register)", async () => {
  const spec = trustSpec();
  const root = tempRoot("rk-ios-verdict-");
  const probe = validateIosPackageConfig(baseSpec(spec));
  const plannerRefuses = (() => {
    try { realRunner(root).plan("xcodebuild", archiveArgv(probe, { root, buildNumber: "2026101801", trusted: assertTrustedPins(parsePackageResolved(resolvedFile(CODERIGHT_PINS)), probe) }), { cwd: root }); return false; } catch (error) { return error.code === "RK_BUILD_LAUNCH_REFUSED"; }
  })();
  const f = fixture({ spec, pins: CODERIGHT_PINS, enforceGrammar: true });
  if (plannerRefuses) {
    await rejects(runIosPackageRecipe(f.deps), "IOS_PACKAGE_NATIVE_REFUSED", /was refused before any launch/);
    assert.equal(f.standIn.launched.length, 0);
    assert.ok(!f.calls.some((call) => call.cmd === "xcodebuild" && call.via === "direct" && call.args.includes("archive")), "no direct xcodebuild archive");
  } else {
    await runIosPackageRecipe(f.deps);
  }
});

test("an output the broker does not report is never guessed: no archive role, an export that read another archive, no or several IPAs fail closed and clean up", async () => {
  const noRoles = fixture({ outputsOf: () => null });
  await rejects(runIosPackageRecipe(noRoles.deps), "IOS_PACKAGE_ARCHIVE_FAILED", /did not report the managed archive location/);
  assertCleaned(noRoles);
  const problem = fixture({ outputsOf: () => ({ ok: false, problem: "the receipt carries no managedOutputs" }) });
  await rejects(runIosPackageRecipe(problem.deps), "IOS_PACKAGE_ARCHIVE_FAILED", /the receipt carries no managedOutputs/);
  const other = fixture({ outputsOf: ({ tool, byRole, state, mapped }) => ({ ok: true, source: "receipt", state, outputs: mapped.outputs, byRole: byRole.export ? { ...byRole, archive: path.join(state, "archive", "Other.xcarchive") } : byRole }) });
  await rejects(runIosPackageRecipe(other.deps), "IOS_PACKAGE_EXPORT_FAILED", /not the verified archive/);
  assertCleaned(other);
  const several = fixture({ extraIpa: true });
  await rejects(runIosPackageRecipe(several.deps), "IOS_PACKAGE_EXPORT_FAILED", /exactly one \.ipa in the managed export directory, found 2/);
  assertCleaned(several);
});

test("QA and release scans run over the archived app and the app inside the IPA, as pre-pack scans", async () => {
  const f = fixture();
  await runIosPackageRecipe(f.deps);
  assert.deepEqual(f.scans.map((scan) => scan.label), ["ios archive app", "ios IPA app"]);
  for (const scan of f.scans) assert.deepEqual(scan.scanOptions, { requireBinary: true, prePack: true });
  assert.ok(slashed(f.scans[0].files[0]).endsWith("Products/Applications/Pulse.app"));
});

test("the build number rises across runs on the same day", async () => {
  const f = fixture();
  await runIosPackageRecipe(f.deps);
  await runIosPackageRecipe(f.deps);
  const builds = f.calls.filter((call) => call.args.includes("archive")).map((call) => call.args.find((arg) => arg.startsWith("CURRENT_PROJECT_VERSION=")));
  assert.deepEqual(builds, ["CURRENT_PROJECT_VERSION=2026101801", "CURRENT_PROJECT_VERSION=2026101802"]);
});

test("xcodegen is required when a project.yml is named: a missing tool fails and the committed project is never used", async () => {
  const f = fixture({ noXcodegen: true });
  mkdirSync(path.join(f.root, "ios", "Pulse.xcodeproj"), { recursive: true }); // a committed project must not rescue the run
  await rejects(runIosPackageRecipe(f.deps), "IOS_PACKAGE_TOOL_MISSING", /xcodegen is required/);
  assert.ok(!f.calls.some((call) => call.args.includes("archive")));
  assert.ok(!existsSync(f.ipa));
});

test("without a projectSpec the named project must exist and xcodegen is not needed", async () => {
  const f = fixture({ spec: { projectSpec: undefined } });
  await rejects(runIosPackageRecipe(f.deps), "IOS_PACKAGE_CONFIG_INVALID", /does not exist/);
  mkdirSync(path.join(f.root, "ios", "Pulse.xcodeproj"), { recursive: true });
  await runIosPackageRecipe(f.deps);
  assert.ok(!f.calls.some((call) => call.cmd === "xcodegen"));
});

test("a missing xcodebuild is reported before anything is written", async () => {
  const f = fixture({ deps: { run: (cmd) => (cmd === "xcodebuild" ? { status: 127, error: new Error("ENOENT") } : { status: 0, stdout: "" }) } });
  await rejects(runIosPackageRecipe(f.deps), "IOS_PACKAGE_TOOL_MISSING", /xcodebuild/);
  assert.ok(!existsSync(path.join(f.root, ".right-release")));
});

function assertCleaned(f) {
  assert.ok(!existsSync(f.ipa), "the IPA is deleted");
  assert.ok(!existsSync(f.receipt), "no receipt is left for a seal to find");
}

test("every failure after the build starts deletes the IPA and the receipt (the broker-owned archive and export are the broker's to reap)", async () => {
  const cases = [
    ["archive fails", { fail: "xcodebuild archive" }, "IOS_PACKAGE_ARCHIVE_FAILED"],
    ["export fails", { fail: "xcodebuild -exportArchive" }, "IOS_PACKAGE_EXPORT_FAILED"],
    ["unzip fails", { fail: "ditto -x" }, "IOS_PACKAGE_VERIFY_FAILED"],
    ["codesign verify fails", { badSignature: true }, "IOS_PACKAGE_VERIFY_FAILED"],
    ["prepare fails", { fail: "node scripts/prep.mjs", spec: { prepare: [{ cmd: "node", args: ["scripts/prep.mjs"] }] } }, "IOS_PACKAGE_PREPARE_FAILED"],
    ["xcodegen generate fails", { fail: "xcodegen generate" }, "IOS_PACKAGE_GENERATE_FAILED"],
  ];
  for (const [label, over, code] of cases) {
    const f = fixture(over);
    mkdirSync(path.dirname(f.ipa), { recursive: true });
    writeFileSync(f.ipa, "stale");
    mkdirSync(path.dirname(f.receipt), { recursive: true });
    writeFileSync(f.receipt, "{}");
    await rejects(runIosPackageRecipe(f.deps), code);
    assertCleaned(f);
    assert.ok(label);
  }
});

test("verification failures are fail-closed and clean up: drift, bundle id, plist, entitlements, profile, forbidden strings, scans", async () => {
  const cases = [
    ["Watch/appex version drift", { appexInfo: { CFBundleShortVersionString: "9.9.9" } }, /version drift/],
    ["literal build number overrides the recipe's", { archiveBuild: "0.1.0" }, /version drift|not the recipe's/],
    ["wrong bundle id", { archiveInfo: { CFBundleIdentifier: "dev.other.app" } }, /bundle id/],
    ["Info.plist check", { spec: { checks: { plist: { ITSAppUsesNonExemptEncryption: false } } }, archiveInfo: { ITSAppUsesNonExemptEncryption: true } }, /ITSAppUsesNonExemptEncryption/],
    ["debuggable entitlements", { entitlements: { "get-task-allow": true } }, /get-task-allow/],
    ["development APNs", { entitlements: { "aps-environment": "development" } }, /production/],
    ["development profile", { profile: { TeamIdentifier: [TEAM], Entitlements: { "application-identifier": `${TEAM}.${BUNDLE}` }, ProvisionedDevices: ["u"] } }, /development, ad hoc or enterprise/],
    ["wrong team in the profile", { profile: { TeamIdentifier: ["OTHERTEAM1"], Entitlements: { "application-identifier": `OTHERTEAM1.${BUNDLE}` } } }, /not for team/],
    ["forbidden string", { spec: { checks: { forbidStrings: ["QA_CONTROL_ON"] } }, binary: "xx QA_CONTROL_ON xx" }, /forbidden string/],
    ["a scan refuses the build", { onScan: () => { throw new Error("qa-control found"); } }, /qa-control found/],
    ["drift only inside the IPA", { mutateIpa: (app) => writeFileSync(path.join(app, "PlugIns", "Share.appex", "Info.plist"), JSON.stringify({ CFBundleIdentifier: `${BUNDLE}.share`, CFBundleShortVersionString: "0.2.0", CFBundleVersion: "2026101801" })) }, /version drift/],
  ];
  for (const [label, over, pattern] of cases) {
    const f = fixture(over);
    await assert.rejects(runIosPackageRecipe(f.deps), (error) => error instanceof IosPackageError && pattern.test(error.message), label);
    assertCleaned(f);
  }
});

test("a repository HEAD that moves during the build refuses the export", async () => {
  const f = fixture();
  let reads = 0;
  await rejects(runIosPackageRecipe({ ...f.deps, gitHead: () => { reads += 1; return reads === 1 ? "a".repeat(40) : "b".repeat(40); } }), "IOS_PACKAGE_SOURCE_CHANGED");
  assertCleaned(f);
});

test("a stale IPA from an earlier build is removed first, and a renamed export is adopted", async () => {
  const f = fixture({ ipaName: "Pulse-Release.ipa" });
  mkdirSync(path.dirname(f.ipa), { recursive: true });
  writeFileSync(f.ipa, "stale-ipa");
  const result = await runIosPackageRecipe(f.deps);
  assert.ok(readFileSync(f.ipa, "utf8").startsWith("IPA-BYTES-"));
  assert.equal(result.receipt.file.name, "Pulse.ipa");
  assert.deepEqual(readdirSync(path.dirname(f.ipa)), ["Pulse.ipa"]);
});

test("failure output is trimmed and carries no bearer token", async () => {
  const f = fixture({ fail: "xcodebuild archive" });
  await assert.rejects(runIosPackageRecipe(f.deps), (error) => error.code === "IOS_PACKAGE_ARCHIVE_FAILED" && !error.message.includes("abcdefghijklmnop") && /Bearer \[redacted\]/.test(error.message));
});

test("a receipt that does not describe the IPA is rejected at the seal", async () => {
  const f = fixture();
  await runIosPackageRecipe(f.deps);
  writeFileSync(f.ipa, "tampered");
  assert.throws(() => verifyIosReceipt(f.receipt, f.ipa), /differ from the build receipt/);
  assert.throws(() => verifyIosReceipt(f.receipt, path.join(path.dirname(f.ipa), "Other.ipa")), /different file/);
  assert.deepEqual(iosSealEntry({ file: { name: "A.ipa", sha256: "x", sizeBytes: 1 } }), { role: "ipa", name: "A.ipa", sha256: "x", sizeBytes: 1 });
});

test("a signed extension with no entitlements is an empty set, not a failure", async () => {
  const f = fixture({ emptyAppexEntitlements: true });
  const result = await runIosPackageRecipe(f.deps);
  assert.equal(result.receipt.bundles.length, 3);
});

test("a relative app root is refused", async () => {
  const f = fixture();
  await rejects(runIosPackageRecipe({ ...f.deps, root: "relative/app" }), "IOS_PACKAGE_CONFIG_INVALID", /absolute/);
});

// ---- P1/P2 hardening ----------------------------------------------------------------------------------------------
test("the build number counter lives in the given state directory, not in the app root, and survives a repeated run", async () => {
  const f = fixture();
  await runIosPackageRecipe(f.deps);
  assert.deepEqual(readdirSync(f.deps.counterDir), [`ios-build-number.${BUNDLE}.json`]);
  assert.ok(!existsSync(path.join(f.root, ".right-release", "state")), "nothing in root/.right-release/state");
});

test("a corrupt counter stops the run before the archive and leaves nothing behind", async () => {
  const f = fixture();
  mkdirSync(f.deps.counterDir, { recursive: true });
  writeFileSync(path.join(f.deps.counterDir, `ios-build-number.${BUNDLE}.json`), "{garbage");
  await rejects(runIosPackageRecipe(f.deps), "IOS_PACKAGE_BUILD_NUMBER", /not valid JSON/);
  assert.ok(!f.calls.some((call) => call.args.includes("archive")));
  assert.ok(!existsSync(f.ipa));
});

test("the export is the broker's managed directory: the IPA is copied out beside the app's own files, which are never touched, and no -exportPath is named", async () => {
  const f = fixture({ spec: { ipa: "build/ios/Pulse.ipa" } });
  await runIosPackageRecipe(f.deps);
  const exported = f.calls.find((call) => call.args[0] === "-exportArchive").args;
  assert.ok(!exported.includes("-exportPath") && !exported.includes("-archivePath"));
  assert.ok(existsSync(path.join(f.root, "build", "ios", "Pulse.ipa")));
  assert.deepEqual(readdirSync(path.join(f.root, "build", "ios")), ["Pulse.ipa"], "no export or archive directory beside the IPA");
  // a neighbour in the ipa directory is never touched by the export or its cleanup
  const g = fixture({ spec: { ipa: "build/ios/Pulse.ipa" } });
  mkdirSync(path.join(g.root, "build", "ios"), { recursive: true });
  writeFileSync(path.join(g.root, "build", "ios", "keep.txt"), "mine");
  await runIosPackageRecipe(g.deps);
  assert.equal(readFileSync(path.join(g.root, "build", "ios", "keep.txt"), "utf8"), "mine");
  const h = fixture({ spec: { ipa: "build/ios/Pulse.ipa" }, fail: "xcodebuild -exportArchive" });
  mkdirSync(path.join(h.root, "build", "ios"), { recursive: true });
  writeFileSync(path.join(h.root, "build", "ios", "keep.txt"), "mine");
  await rejects(runIosPackageRecipe(h.deps), "IOS_PACKAGE_EXPORT_FAILED");
  assert.equal(readFileSync(path.join(h.root, "build", "ios", "keep.txt"), "utf8"), "mine", "a failed export leaves the neighbour alone too");
  assert.deepEqual(readdirSync(path.join(h.root, "build", "ios")), ["keep.txt"]);
});

test("a native prepare step (a tool the door knows) is a broker adapter job; the app's own commands stay direct", async () => {
  const f = fixture({ spec: { prepare: [{ cmd: "xcodegen", args: ["generate", "--spec", "ios/project.yml", "--project", "ios"] }, { cmd: "node", args: ["scripts/prep.mjs"] }] } });
  await runIosPackageRecipe(f.deps);
  const prepared = f.calls.filter((call) => call.cmd === "xcodegen" && call.args[0] === "generate");
  assert.deepEqual(prepared.map((call) => call.via), ["native", "native"], "the generate step and the native prepare step");
  assert.equal(f.calls.find((call) => call.cmd === "node").via, "direct");
});

test("every bundle in the IPA carries its own distribution profile: a missing or foreign appex or watch profile fails the run", async () => {
  const ok = fixture();
  const result = await runIosPackageRecipe(ok.deps);
  assert.ok(result.sealEntry);
  const profiles = ok.calls.filter((call) => call.cmd === "security" && call.args[0] === "cms");
  assert.equal(profiles.length, 3, "app, appex and watch are all checked");
  const missing = fixture({ noAppexProfile: true });
  await rejects(runIosPackageRecipe(missing.deps), "IOS_PACKAGE_VERIFY_FAILED", /appex Share\.appex has no embedded\.mobileprovision/);
  assert.ok(!existsSync(missing.ipa) && !existsSync(missing.receipt));
  const foreign = fixture({ profileBundle: { "Share.appex": "dev.other.app" } });
  await rejects(runIosPackageRecipe(foreign.deps), "IOS_PACKAGE_VERIFY_FAILED", /authorizes/);
  const watch = fixture({ profileBundle: { "Pulse Watch.app": "dev.other.watch" } });
  await rejects(runIosPackageRecipe(watch.deps), "IOS_PACKAGE_VERIFY_FAILED", /authorizes/);
});

test("command output in errors masks keys, bearer tokens, bare JWTs and long base64 runs but keeps ordinary paths", () => {
  const jwt = "eyJhbGciOiJFUzI1NiIsImtpZCI6IkFCQzEyMyJ9.eyJpc3MiOiJ4In0.c2lnbmF0dXJlYnl0ZXM";
  const der = Buffer.alloc(138, 7).toString("base64");
  const slashy = Buffer.from("\xfb\xff\xbe".repeat(60), "latin1").toString("base64");
  assert.ok(slashy.includes("/") || slashy.includes("+"));
  const text = outputTail({ stdout: `token ${jwt}\nkey ${der}\nraw ${slashy}\nhash ${"a".repeat(64)}\n${"-----BEGIN " + "PRIVATE KEY-----"}\nabc\n-----END PRIVATE KEY-----\nBearer abcdefghijklmnop\nat /Users/me/Library/Developer/Xcode/DerivedData/Pulse-abcdefghijklmnopqrstuvwxyz/Build/Products/Release-iphoneos/Pulse.app\n`, stderr: "" });
  for (const secret of [jwt, der, slashy, "a".repeat(64), "zzyyxxwwvvuuttss", "BEGIN PRIVATE KEY"]) assert.ok(!text.includes(secret), `leaked ${secret.slice(0, 12)}`);
  assert.match(text, /\/Users\/me\/Library\/Developer\/Xcode\/DerivedData\/Pulse-abcdefghijklmnopqrstuvwxyz\/Build\/Products\/Release-iphoneos\/Pulse\.app/);
});

test("the seal-time receipt must be for the commit being sealed", async () => {
  const f = fixture();
  const result = await runIosPackageRecipe(f.deps);
  assert.equal(JSON.parse(readFileSync(f.receipt, "utf8")).sourceCommit, "a".repeat(40));
  assert.ok(result.sealEntry);
});

test("a provisioning profile's dates and certificate data convert to JSON (plutil refuses both as-is)", { skip: process.platform !== "darwin" }, async () => {
  const { spawnSync } = await import("./background-process.mjs");
  const xml = `<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Name</key><string>iOS Team Store Provisioning Profile: app.example</string>
<key>UUID</key><string>00000000-0000-0000-0000-000000000000</string>
<key>CreationDate</key><date>2026-10-19T00:00:00Z</date>
<key>ExpirationDate</key><date>2027-10-19T00:00:00Z</date>
<key>DeveloperCertificates</key><array><data>
MIIF</data></array>
<key>TeamIdentifier</key><array><string>${TEAM}</string></array>
<key>Entitlements</key><dict><key>application-identifier</key><string>${TEAM}.app.example</string><key>get-task-allow</key><false/></dict>
</dict></plist>`;
  assert.notEqual(spawnSync("plutil", ["-convert", "json", "-o", "-", "-"], { input: xml }).status, 0);
  const converted = spawnSync("plutil", ["-convert", "json", "-o", "-", "-"], { input: jsonSafePlistText(xml), encoding: "utf8" });
  assert.equal(converted.status, 0, converted.stderr);
  const summary = assertDistributionProfile(JSON.parse(converted.stdout), { teamId: TEAM, bundleId: "app.example" });
  assert.equal(summary.expiration, "2027-10-19T00:00:00Z");
});

// ---- trusted Swift macros / plugins: the run -----------------------------------------------------------------------
const DEFAULT_PINS_PATH = "ios/Pulse.xcodeproj/project.xcworkspace/xcshareddata/swiftpm/Package.resolved";

test("matching pins add -skipMacroValidation plus the resolution locks to the archive only; export gets neither; the receipt records what was trusted", async () => {
  const f = fixture({ spec: trustSpec(), pins: CODERIGHT_PINS });
  await runIosPackageRecipe(f.deps);
  const archive = archiveCall(f).args;
  assert.ok(archive.includes("-skipMacroValidation"));
  assert.ok(!archive.includes("-skipPackagePluginValidation"), "no plugin flag without a trustedSwiftPlugins list");
  assert.ok(archive.includes("-disableAutomaticPackageResolution") && archive.includes("-onlyUsePackageVersionsFromResolvedFile"));
  assert.equal(archive.at(-1), "archive");
  const exported = f.calls.find((call) => call.args[0] === "-exportArchive").args;
  assert.ok(!exported.some((arg) => arg.startsWith("-skip") || arg.startsWith("-disableAutomatic") || arg.startsWith("-onlyUse")));
  const receipt = JSON.parse(readFileSync(f.receipt, "utf8"));
  const bytes = readFileSync(path.join(f.root, "Package.resolved"));
  assert.equal(receipt.swiftTrust.packageResolved.sha256, sha(bytes));
  assert.equal(receipt.swiftTrust.packageResolved.path, "Package.resolved");
  assert.deepEqual(receipt.swiftTrust.macros.map((m) => [m.package, m.revision]), Object.entries(CODERIGHT_PINS));
  assert.deepEqual(receipt.swiftTrust.plugins, []);
  // The committed file was copied into the generated project, and the trust check ran before xcodegen touched anything.
  assert.equal(sha(readFileSync(path.join(f.root, DEFAULT_PINS_PATH))), sha(bytes));
  const order = f.calls.map((call) => `${call.cmd}:${call.args[0]}`);
  assert.ok(order.indexOf("git:ls-files") < order.indexOf("xcodegen:generate"));
});

test("a plugin list adds -skipPackagePluginValidation under the same rules", async () => {
  const plugin = { package: "swiftlint-plugin", revision: "2".repeat(40) };
  const f = fixture({ spec: trustSpec({ trustedSwiftMacros: undefined, trustedSwiftPlugins: [plugin] }), pins: { ...CODERIGHT_PINS, "swiftlint-plugin": plugin.revision } });
  await runIosPackageRecipe(f.deps);
  const archive = archiveCall(f).args;
  assert.ok(archive.includes("-skipPackagePluginValidation") && !archive.includes("-skipMacroValidation"));
  assert.deepEqual(JSON.parse(readFileSync(f.receipt, "utf8")).swiftTrust.plugins.map((p) => p.package), ["swiftlint-plugin"]);
  const bad = fixture({ spec: trustSpec({ trustedSwiftMacros: undefined, trustedSwiftPlugins: [plugin] }), pins: { ...CODERIGHT_PINS, "swiftlint-plugin": "3".repeat(40) } });
  await rejects(runIosPackageRecipe(bad.deps), "IOS_PACKAGE_TRUST_MISMATCH", /swiftlint-plugin is pinned to 3{40}/);
  assert.equal(bad.calls.some((call) => call.cmd === "xcodebuild" && call.args.includes("archive")), false);
});

test("a revision mismatch or a package missing from Package.resolved is refused before xcodegen or any build, naming each package", async () => {
  const moved = { ...CODERIGHT_PINS, "swift-dependencies": "9".repeat(40) };
  delete moved["swift-navigation"];
  const f = fixture({ spec: trustSpec(), pins: moved });
  await rejects(runIosPackageRecipe(f.deps), "IOS_PACKAGE_TRUST_MISMATCH", /swift-dependencies is pinned to 9{40}[^]*swift-navigation is not pinned/);
  assert.ok(!f.calls.some((call) => call.cmd === "xcodegen" && call.args[0] === "generate"));
  assert.ok(!f.calls.some((call) => call.cmd === "xcodebuild" && call.args.includes("archive")));
  assert.ok(!existsSync(f.receipt) && !existsSync(f.ipa));
});

test("Package.resolved missing, untracked, modified, unreadable or the wrong format version is refused", async () => {
  const missing = fixture({ spec: trustSpec() });
  await rejects(runIosPackageRecipe(missing.deps), "IOS_PACKAGE_TRUST_FAILED", /Package\.resolved does not exist/);
  const untracked = fixture({ spec: trustSpec(), pins: CODERIGHT_PINS, untrackedPins: true });
  await rejects(runIosPackageRecipe(untracked.deps), "IOS_PACKAGE_TRUST_FAILED", /not tracked by git/);
  const dirty = fixture({ spec: trustSpec(), pins: CODERIGHT_PINS, dirtyPins: true });
  await rejects(runIosPackageRecipe(dirty.deps), "IOS_PACKAGE_TRUST_FAILED", /differs from HEAD/);
  const v1 = fixture({ spec: trustSpec(), pins: JSON.stringify({ version: 1, object: { pins: [] } }) });
  await rejects(runIosPackageRecipe(v1.deps), "IOS_PACKAGE_TRUST_FAILED", /version 1 is not supported/);
  for (const f of [missing, untracked, dirty, v1]) assert.ok(!f.calls.some((call) => call.cmd === "xcodebuild" && call.args.includes("archive")));
});

test("without a projectSpec the in-xcodeproj Package.resolved is the tracked source, and nothing is copied", async () => {
  const f = fixture({ spec: { projectSpec: undefined, trustedSwiftMacros: trustList() }, pins: CODERIGHT_PINS, pinsPath: DEFAULT_PINS_PATH });
  mkdirSync(path.join(f.root, "ios", "Pulse.xcodeproj"), { recursive: true });
  await runIosPackageRecipe(f.deps);
  assert.ok(archiveCall(f).args.includes("-skipMacroValidation"));
  assert.equal(JSON.parse(readFileSync(f.receipt, "utf8")).swiftTrust.packageResolved.path, DEFAULT_PINS_PATH);
  const missing = fixture({ spec: { projectSpec: undefined, trustedSwiftMacros: trustList() } });
  mkdirSync(path.join(missing.root, "ios", "Pulse.xcodeproj"), { recursive: true });
  await rejects(runIosPackageRecipe(missing.deps), "IOS_PACKAGE_TRUST_FAILED", /does not exist/);
});

test("a different Package.resolved already inside the generated project is refused, never overwritten; an identical one is accepted", async () => {
  const f = fixture({ spec: trustSpec(), pins: CODERIGHT_PINS });
  const inProject = path.join(f.root, DEFAULT_PINS_PATH);
  mkdirSync(path.dirname(inProject), { recursive: true });
  writeFileSync(inProject, resolvedFile({ ...CODERIGHT_PINS, "swift-case-paths": "e".repeat(40) }));
  await rejects(runIosPackageRecipe(f.deps), "IOS_PACKAGE_TRUST_FAILED", /already exists and differs/);
  assert.match(readFileSync(inProject, "utf8"), /e{40}/);
  assert.ok(!archiveCall(f));
  const same = fixture({ spec: trustSpec(), pins: CODERIGHT_PINS });
  const sameFile = path.join(same.root, DEFAULT_PINS_PATH);
  mkdirSync(path.dirname(sameFile), { recursive: true });
  writeFileSync(sameFile, readFileSync(path.join(same.root, "Package.resolved")));
  await runIosPackageRecipe(same.deps);
  assert.ok(archiveCall(same));
});

test("pins that change between the check and the archive, or during the archive, fail the run and delete the outputs", async () => {
  const f = fixture({ spec: trustSpec({ prepare: [{ cmd: "node", args: ["rewrite.mjs"] }] }), pins: CODERIGHT_PINS });
  const inner = f.deps.run;
  const tamper = (cmd, args, options) => {
    if (cmd === "node") { const file = path.join(f.root, DEFAULT_PINS_PATH); mkdirSync(path.dirname(file), { recursive: true }); writeFileSync(file, "{}"); }
    return inner(cmd, args, options);
  };
  await rejects(runIosPackageRecipe({ ...f.deps, run: tamper }), "IOS_PACKAGE_TRUST_FAILED", /already exists and differs/);
  const g = fixture({ spec: trustSpec(), pins: CODERIGHT_PINS });
  const moveDuringArchive = { ...g.standIn, run: (cmd, args, options) => {
    const result = g.standIn.run(cmd, args, options);
    if (cmd === "xcodebuild" && args.includes("archive")) writeFileSync(path.join(g.root, DEFAULT_PINS_PATH), resolvedFile({ ...CODERIGHT_PINS, "swift-perception": "d".repeat(40) }));
    return result;
  } };
  await rejects(runIosPackageRecipe({ ...g.deps, native: moveDuringArchive }), "IOS_PACKAGE_TRUST_FAILED", /differs from the verified Package\.resolved/);
  assert.ok(!existsSync(g.ipa) && !existsSync(g.receipt));
});

test("the dry run shows the trust step and the flags without reading the pins file", async () => {
  const f = fixture({ spec: trustSpec() });
  const lines = [];
  const result = await runIosPackageRecipe({ ...f.deps, platform: "win32", dryRun: true, run: () => assert.fail("a dry run runs nothing"), log: (line) => lines.push(line) });
  assert.ok(result.plan.steps.some((step) => step.id === "trust-check"));
  assert.ok(result.plan.steps.find((step) => step.id === "archive").args.includes("-skipMacroValidation"));
  assert.ok(lines.some((line) => /trust-check/.test(line)));
});
