import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import nodeFs, { cpSync, existsSync, mkdirSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import path from "node:path";
import test from "node:test";
import { tempRoot } from "./test-support/temp-roots.mjs";
import {
  IOS_PACKAGE_STEPS, IOS_RECEIPT_NAME, IOS_SIGNING_CONTRACT, IosPackageError, archiveArgv, assertDistributionEntitlements, assertDistributionProfile,
  assertVersionIdentity, exportArgv, exportOptionsPlist, iosPackagePlan, iosSealEntry, nextDateBuildNumber, runIosPackageRecipe, validateIosPackageConfig,
  jsonSafePlistText, verifyIosReceipt, xcodegenArgv,
} from "./ios-package-recipe.mjs";
import { outputTail } from "./ios-host.mjs";

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

test("the archive command carries the configured team, automatic provisioning and the recipe's build number, and no ASC credential", () => {
  const spec = validateIosPackageConfig(baseSpec({ extraBuildSettings: { HR_FLAG: "1" } }));
  const root = path.resolve("fixture-app");
  const args = archiveArgv(spec, { root, buildNumber: "2026101801", storage: { derivedData: "/managed/DerivedData", resultBundle: "/managed/r.xcresult" } });
  assert.deepEqual(args.slice(0, 2), ["-project", path.join(root, "ios", "Pulse.xcodeproj")]);
  for (const expected of [`DEVELOPMENT_TEAM=${TEAM}`, "CURRENT_PROJECT_VERSION=2026101801", "HR_FLAG=1", "-allowProvisioningUpdates", "archive"]) assert.ok(args.includes(expected), expected);
  assert.equal(args[args.indexOf("-derivedDataPath") + 1], "/managed/DerivedData");
  assert.equal(args[args.indexOf("-resultBundlePath") + 1], "/managed/r.xcresult");
  assert.equal(args[args.indexOf("-destination") + 1], "generic/platform=iOS");
  assert.equal(args.at(-1), "archive");
  assert.ok(!args.some((arg) => /authenticationKey|AuthKey|\.p8/i.test(arg)));
  assert.deepEqual(xcodegenArgv(spec, root), ["generate", "--spec", path.join(root, "ios", "project.yml"), "--project", path.join(root, "ios")]);
  assert.ok(exportArgv(spec, { root, optionsFile: "/x/o.plist" }).includes("-exportArchive"));
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

// ---- the run, with every dependency injected ----------------------------------------------------------------------
function fixture(over = {}) {
  const root = tempRoot("rk-ios-recipe-");
  mkdirSync(path.join(root, "ios"), { recursive: true });
  writeFileSync(path.join(root, "ios", "project.yml"), "name: Pulse\n");
  const spec = validateIosPackageConfig(baseSpec(over.spec));
  const managed = path.join(root, "managed");
  const calls = [];
  const scans = [];
  let head = "a".repeat(40);
  const bundleInfo = (build, extra = {}) => ({ CFBundleIdentifier: BUNDLE, CFBundleShortVersionString: "0.1.0", CFBundleVersion: build, ITSAppUsesNonExemptEncryption: false, ...extra });
  const writeBundle = (dir, info, binary = "fake-macho") => { mkdirSync(dir, { recursive: true }); writeFileSync(path.join(dir, "Info.plist"), JSON.stringify(info)); writeFileSync(path.join(dir, "exe"), binary); };
  const ok = (stdout = "") => ({ status: 0, stdout, stderr: "" });
  let lastArchive;
  const run = (cmd, args, options = {}) => {
    calls.push({ cmd, args: [...args], cwd: options.cwd, input: options.input });
    const id = `${cmd} ${args[0]}`;
    if (over.fail === id || (over.fail === "xcodebuild archive" && args.includes("archive"))) return { status: 1, stdout: "", stderr: "boom: sensitive Bearer abcdefghijklmnop" };
    if (cmd === "xcodebuild" && args[0] === "-version") return ok("Xcode 26.0\nBuild version 17A324\n");
    if (cmd === "xcodegen" && args[0] === "--version") return over.noXcodegen ? { status: 127, error: new Error("spawn xcodegen ENOENT") } : ok("2.43.0");
    if (cmd === "xcodegen" && args[0] === "generate") { mkdirSync(path.join(root, "ios", "Pulse.xcodeproj"), { recursive: true }); return ok(); }
    if (cmd === "node") return ok();
    if (cmd === "xcodebuild" && args.includes("archive")) {
      const archive = args[args.indexOf("-archivePath") + 1];
      const build = args.find((arg) => arg.startsWith("CURRENT_PROJECT_VERSION=")).split("=")[1];
      const app = path.join(archive, "Products", "Applications", "Pulse.app");
      writeBundle(app, bundleInfo(over.archiveBuild ?? build, over.archiveInfo), over.binary);
      writeBundle(path.join(app, "PlugIns", "Share.appex"), bundleInfo(build, { CFBundleIdentifier: `${BUNDLE}.share`, ...(over.appexInfo ?? {}) }));
      writeBundle(path.join(app, "Watch", "Pulse Watch.app"), bundleInfo(build, { CFBundleIdentifier: `${BUNDLE}.watchkitapp` }));
      lastArchive = archive;
      return ok("** ARCHIVE SUCCEEDED **");
    }
    if (cmd === "xcodebuild" && args[0] === "-exportArchive") {
      const dir = args[args.indexOf("-exportPath") + 1];
      mkdirSync(dir, { recursive: true });
      writeFileSync(path.join(dir, over.ipaName ?? "Pulse.ipa"), `IPA-BYTES-${calls.length}`);
      return ok("** EXPORT SUCCEEDED **");
    }
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
    return { status: 1, stdout: "", stderr: `unexpected command ${cmd} ${args.join(" ")}` };
  };
  const deps = {
    root, spec, env: { PATH: "/usr/bin" }, platform: "darwin", run, now: () => NOW, log: () => {}, tmpRoot: root, counterDir: path.join(root, "vault-state", "ios-build-numbers"),
    detectSession: () => ({ guiRunner: false, reason: "Aqua security session" }),
    storage: () => ({ derivedData: path.join(managed, "DerivedData"), resultBundle: path.join(managed, "r.xcresult") }),
    gitHead: () => head,
    scanFiles: (label, files, scanOptions) => { scans.push({ label, files, scanOptions }); over.onScan?.(label); },
    ...over.deps,
  };
  return { root, spec, calls, scans, deps, setHead: (value) => { head = value; }, ipa: path.join(root, "build", "ios", "export", "Pulse.ipa"), archive: path.join(root, "build", "ios", "Pulse.xcarchive"), receipt: path.join(root, ".right-release", "receipts", IOS_RECEIPT_NAME) };
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
  assert.ok(existsSync(path.join(f.root, ".right-release", "receipts", "ios-export-options.plist")));
});

test("archive and export run with automatic provisioning, the configured team and no App Store Connect credential", async () => {
  const f = fixture({ spec: { extraBuildSettings: { HR_FLAG: "1" } } });
  await runIosPackageRecipe({ ...f.deps, env: { PATH: "/usr/bin", APPLE_API_KEY_PATH: "/Users/me/AuthKey_X.p8" } });
  const archive = f.calls.find((call) => call.cmd === "xcodebuild" && call.args.includes("archive")).args;
  const exported = f.calls.find((call) => call.args[0] === "-exportArchive").args;
  for (const args of [archive, exported]) {
    assert.ok(args.includes("-allowProvisioningUpdates"));
    assert.ok(!args.some((arg) => /authenticationKey|AuthKey|\.p8/i.test(arg)));
  }
  assert.ok(archive.includes(`DEVELOPMENT_TEAM=${TEAM}`) && archive.includes("CURRENT_PROJECT_VERSION=2026101801") && archive.includes("HR_FLAG=1"));
  assert.equal(slashed(archive[archive.indexOf("-derivedDataPath") + 1]), slashed(path.join(f.root, "managed", "DerivedData")));
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
  assert.ok(!existsSync(f.archive), "the archive is deleted");
  assert.ok(!existsSync(f.receipt), "no receipt is left for a seal to find");
}

test("every failure after the build starts deletes the archive, the IPA and the receipt", async () => {
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

test("export goes to a dedicated subdirectory: the app root's ipa directory holds only the final IPA, and the export directory is gone", async () => {
  const f = fixture({ spec: { ipa: "build/ios/Pulse.ipa" } });
  await runIosPackageRecipe(f.deps);
  const exported = f.calls.find((call) => call.args[0] === "-exportArchive").args;
  const exportPath = exported[exported.indexOf("-exportPath") + 1];
  assert.notEqual(path.resolve(exportPath), path.join(f.root, "build", "ios"), "never the directory that holds the IPA and the archive");
  assert.equal(path.dirname(path.resolve(exportPath)), path.join(f.root, "build", "ios"));
  assert.ok(!existsSync(exportPath), "the export directory is removed after success");
  assert.ok(existsSync(path.join(f.root, "build", "ios", "Pulse.ipa")));
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
  const text = outputTail({ stdout: `token ${jwt}\nkey ${der}\nraw ${slashy}\nhash ${"a".repeat(64)}\n-----BEGIN PRIVATE KEY-----\nabc\n-----END PRIVATE KEY-----\nBearer abcdefghijklmnop\nat /Users/me/Library/Developer/Xcode/DerivedData/Pulse-abcdefghijklmnopqrstuvwxyz/Build/Products/Release-iphoneos/Pulse.app\n`, stderr: "" });
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
  const { spawnSync } = await import("node:child_process");
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
