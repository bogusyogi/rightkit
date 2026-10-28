import assert from "node:assert/strict";
import path from "node:path";
import test from "node:test";
import {
  buildAndEmbedNativeHelpers, buildEmbedSignNativeHelpers, helperInfoPlist, nativeHelperEntitlementMap, nativeHelperNestedCode,
  nativeHelpersFromConfig, planNativeHelperSigning, signNativeHelpers, validateNativeHelpers,
} from "./native-helpers.mjs";
import { planMacSigning } from "./mac-signing-matrix.mjs";

const ROOT = "/rk-helpers/app";
const APP = `${ROOT}/dist/Demo.app`;
const BIN = "/rk-helpers/scratch/pkg/.build/apple/Products/Release";
const LOGIN = "Contents/Library/LoginItems";

const menu = (extra = {}) => ({
  name: "menubar", package: "macos-native/Pkg", product: "MenuBar", kind: "app", embed: `${LOGIN}/MenuBar.app`, universal: true,
  info: { bundleId: "com.example.statusbar", uiElement: true },
  resources: [{ from: "src-tauri/icons/tray", to: "tray" }],
  ...extra,
});
const cloud = (extra = {}) => ({
  name: "cloud", sharedPackage: "cloudkit", product: "rightkit-cloudkit-helper", kind: "app", embed: `${LOGIN}/CloudHelper.app`, universal: true,
  executable: "CloudHelper", entitlements: "src-tauri/cloud.plist", provisioningProfile: "src-tauri/p.provisionprofile",
  info: { bundleId: "com.example.next", name: "CloudHelper" },
  ...extra,
});
const tool = (extra = {}) => ({ name: "tool", package: "macos-native/Pkg", product: "tool", kind: "executable", embed: "Contents/MacOS/tool", universal: true, ...extra });

function memoryFs(existing = []) {
  const files = new Map(existing.map((file) => [file, Buffer.from(`bytes:${file}`)]));
  const dirs = new Set();
  const log = [];
  const fs = {
    existsSync: (file) => files.has(file) || dirs.has(file) || [...files.keys(), ...dirs].some((other) => other.startsWith(`${file}/`)),
    mkdirSync: (dir) => { dirs.add(dir); },
    writeFileSync: (file, data) => { log.push(`write:${file}`); files.set(file, Buffer.from(data)); },
    copyFileSync: (from, to) => { log.push(`copy:${from}->${to}`); files.set(to, files.get(from) ?? Buffer.from(`bytes:${from}`)); },
    cpSync: (from, to) => { log.push(`cp:${from}->${to}`); files.set(to, Buffer.from(`res:${from}`)); },
    chmodSync: (file, mode) => { log.push(`chmod:${file}:${mode.toString(8)}`); },
    readFileSync: (file) => files.get(file) ?? Buffer.from(`bytes:${file}`),
    rmSync: (target) => { log.push(`rm:${target}`); for (const key of [...files.keys()]) if (key === target || key.startsWith(`${target}/`)) files.delete(key); },
    readdirSync: () => [],
    mkdtempSync: (prefix) => `${prefix}x`,
  };
  return { fs, files, log };
}

const baseExisting = [`${APP}/Contents/Info.plist`, `${ROOT}/macos-native/Pkg/Package.swift`, `${ROOT}/src-tauri/icons/tray`, `${ROOT}/src-tauri/cloud.plist`, `${ROOT}/src-tauri/p.provisionprofile`, "/rk-rightkit/swift/RightKitCloudKitHelper/Package.swift", `${BIN}/MenuBar`, `${BIN}/tool`, `${BIN}/rightkit-cloudkit-helper`];

function fakeSpawn({ archs = "x86_64 arm64", binDir = BIN } = {}) {
  const calls = [];
  const spawn = (cmd, args, options) => {
    calls.push({ cmd, args, options });
    if (args.includes("--show-bin-path")) return { status: 0, stdout: `${binDir}\n`, stderr: "" };
    if (cmd === "lipo") return { status: 0, stdout: `${archs}\n`, stderr: "" };
    return { status: 0, stdout: "", stderr: "" };
  };
  return { spawn, calls };
}

test("validation is strict: unknown keys, absolute and escaping paths, wrong kind combinations, duplicates and overlaps", () => {
  assert.equal(validateNativeHelpers([menu(), cloud(), tool()]).length, 3);
  const bad = (entry, pattern) => assert.throws(() => validateNativeHelpers([entry]), (error) => error.code === "NATIVE_HELPERS_INVALID" && pattern.test(error.message), pattern.source);
  bad(menu({ extra: 1 }), /unknown key "extra"/);
  bad(menu({ package: "/abs/pkg" }), /relative path/);
  bad(menu({ package: "../pkg" }), /escape|'\.\.'/);
  bad(menu({ entitlements: "../x.plist" }), /entitlements/);
  bad(menu({ entitlements: "x.txt" }), /plist/);
  bad(menu({ embed: "Contents/Library/../../x.app" }), /'\.\.'/);
  bad(menu({ embed: "Contents/Resources/X.app" }), /must be inside the \.app under/);
  bad(menu({ embed: "/Applications/X.app" }), /relative path/);
  bad(menu({ embed: `${LOGIN}/X` }), /\.app/);
  bad(tool({ embed: "Contents/MacOS/sub/tool" }), /kind "executable"/);
  bad(tool({ embed: "Contents/Library/Tool.app" }), /kind "executable"/);
  bad(menu({ package: undefined }), /exactly one of package or sharedPackage/);
  bad(menu({ sharedPackage: "cloudkit" }), /exactly one/);
  bad(cloud({ sharedPackage: "nope" }), /sharedPackage must be one of/);
  bad(menu({ universal: undefined }), /universal is required/);
  bad(menu({ info: undefined }), /info is required/);
  bad(menu({ info: { bundleId: "nodots" } }), /reverse-DNS/);
  bad(tool({ info: { bundleId: "a.b" } }), /only to kind "app"/);
  bad(tool({ provisioningProfile: "p.provisionprofile" }), /only to kind "app"/);
  bad(tool({ executable: "x" }), /only to kind "app"/);
  bad(menu({ resources: [{ from: "a", to: "../b" }] }), /resources\[0\]\.to/);
  bad(menu({ appIntents: { moduleName: "M", extra: 1 } }), /unknown key "extra"/);
  assert.throws(() => validateNativeHelpers([menu(), menu({ name: "menubar", embed: `${LOGIN}/Other.app` })]), /duplicate name/);
  assert.throws(() => validateNativeHelpers([menu(), menu({ name: "b", embed: `${LOGIN}/MenuBar.app` })]), /unique and not nested/);
  assert.throws(() => validateNativeHelpers([menu(), menu({ name: "b", kind: "executable", info: undefined, resources: undefined, embed: `${LOGIN}/MenuBar.app/Contents/MacOS/x` })]), /not nested|nested/);
  assert.throws(() => validateNativeHelpers([]), /non-empty array/);
});

test("signing configuration derived from the list: nestedCode per embed, entitlement map, config lookup", () => {
  const helpers = validateNativeHelpers([menu(), cloud()]);
  assert.deepEqual(nativeHelperNestedCode(helpers), { [`${LOGIN}/MenuBar.app`]: {}, [`${LOGIN}/CloudHelper.app`]: { entitlements: "src-tauri/cloud.plist" } });
  assert.deepEqual(nativeHelperEntitlementMap(helpers), { [`${LOGIN}/CloudHelper.app`]: "src-tauri/cloud.plist" });
  assert.equal(nativeHelpersFromConfig({}), undefined);
  assert.equal(nativeHelpersFromConfig({ targets: { mac: { macPackage: { nativeHelpers: [menu()] } } } }).length, 1);
});

test("Info.plist carries the declared bundle fields, LSUIElement and the executable name", () => {
  const [helper] = validateNativeHelpers([cloud({ info: { bundleId: "com.example.next", name: "CloudHelper", displayName: "Cloud", uiElement: true, minimumSystemVersion: "15.0" } })]);
  const plist = helperInfoPlist({ helper, executable: "CloudHelper", version: "1.2.3" });
  for (const needle of ["<key>CFBundleExecutable</key><string>CloudHelper</string>", "<key>CFBundleIdentifier</key><string>com.example.next</string>", "<key>CFBundleDisplayName</key><string>Cloud</string>", "<key>LSUIElement</key><true/>", "<key>LSMinimumSystemVersion</key><string>15.0</string>", "<key>CFBundleShortVersionString</key><string>1.2.3</string>"]) assert.ok(plist.includes(needle), needle);
  assert.ok(!helperInfoPlist({ helper: validateNativeHelpers([menu({ info: { bundleId: "a.b" } })])[0], executable: "x", version: "1.0.0" }).includes("LSUIElement"));
});

test("builds every helper with explicit --arch slices, finds the bin path, verifies archs exactly, then embeds before anything is signed", () => {
  const { fs, log } = memoryFs(baseExisting);
  const { spawn, calls } = fakeSpawn();
  const result = buildAndEmbedNativeHelpers({ appRoot: ROOT, app: APP, helpers: [menu(), cloud(), tool()], version: "1.2.3", env: {}, spawn, fs, sharedRoot: "/rk-rightkit", packageOwner: (pkg) => pkg.startsWith("/rk-rightkit/") ? "/rk-rightkit" : ROOT });
  assert.deepEqual(result.events, ["build:menubar", "embed:menubar", "build:cloud", "embed:cloud", "build:tool", "embed:tool"]);
  const swift = calls.filter((call) => call.cmd === "swift" && !call.args.includes("--show-bin-path"));
  assert.deepEqual(swift[0].args, ["build", "-c", "release", "--package-path", "macos-native/Pkg", "--product", "MenuBar", "--arch", "arm64", "--arch", "x86_64"]);
  assert.equal(swift[0].options.cwd, ROOT);
  assert.equal(swift[1].args[swift[1].args.indexOf("--package-path") + 1], "swift/RightKitCloudKitHelper");
  assert.equal(swift[1].options.cwd, "/rk-rightkit");
  assert.deepEqual(calls.filter((call) => call.cmd === "lipo")[0].args, ["-archs", `${BIN}/MenuBar`]);
  assert.ok(log.includes(`copy:${BIN}/MenuBar->${APP}/${LOGIN}/MenuBar.app/Contents/MacOS/MenuBar`));
  assert.ok(log.includes(`copy:${BIN}/rightkit-cloudkit-helper->${APP}/${LOGIN}/CloudHelper.app/Contents/MacOS/CloudHelper`), "executable renamed");
  assert.ok(log.includes(`copy:${ROOT}/src-tauri/p.provisionprofile->${APP}/${LOGIN}/CloudHelper.app/Contents/embedded.provisionprofile`));
  assert.ok(log.includes(`cp:${ROOT}/src-tauri/icons/tray->${APP}/${LOGIN}/MenuBar.app/Contents/Resources/tray`));
  assert.ok(log.includes(`copy:${BIN}/tool->${APP}/Contents/MacOS/tool`));
  assert.ok(log.includes(`chmod:${APP}/Contents/MacOS/tool:755`));
  assert.ok(!calls.some((call) => call.cmd === "codesign"), "embedding never signs");
  assert.equal(result.helpers.length, 3);
});

test("a thin forced arch builds one slice; a non-universal helper builds the host arch", () => {
  const thin = fakeSpawn({ archs: "arm64" });
  buildAndEmbedNativeHelpers({ appRoot: ROOT, app: APP, helpers: [menu()], version: "1.0.0", forceArch: "arm64", env: {}, spawn: thin.spawn, fs: memoryFs(baseExisting).fs });
  assert.deepEqual(thin.calls[0].args.slice(-2), ["--arch", "arm64"]);
  const host = fakeSpawn({ archs: "x86_64" });
  buildAndEmbedNativeHelpers({ appRoot: ROOT, app: APP, helpers: [menu({ universal: false })], version: "1.0.0", hostArch: "x86_64", env: {}, spawn: host.spawn, fs: memoryFs(baseExisting).fs });
  assert.deepEqual(host.calls[0].args.slice(-2), ["--arch", "x86_64"]);
});

test("fails closed: a thin slice, a missing product, a failing build, an existing embed path, a missing package or entitlements", () => {
  const run = (options, spawn = fakeSpawn().spawn, existing = baseExisting) => buildAndEmbedNativeHelpers({ appRoot: ROOT, app: APP, helpers: [menu()], version: "1.0.0", env: {}, spawn, fs: memoryFs(existing).fs, ...options });
  assert.throws(() => run({}, fakeSpawn({ archs: "arm64" }).spawn), { code: "NATIVE_HELPERS_ARCH_MISMATCH" });
  assert.throws(() => run({}, fakeSpawn({ archs: "arm64 x86_64 arm64e" }).spawn), { code: "NATIVE_HELPERS_ARCH_MISMATCH" });
  assert.throws(() => run({}, fakeSpawn().spawn, baseExisting.filter((file) => file !== `${BIN}/MenuBar`)), { code: "NATIVE_HELPERS_INPUT_MISSING" });
  assert.throws(() => run({}, () => ({ status: 1 })), { code: "NATIVE_HELPERS_STEP_FAILED" });
  assert.throws(() => run({}, fakeSpawn().spawn, [...baseExisting, `${APP}/${LOGIN}/MenuBar.app/Contents/Info.plist`]), { code: "NATIVE_HELPERS_EMBED_EXISTS" });
  assert.doesNotThrow(() => run({ replaceExisting: true }, fakeSpawn().spawn, [...baseExisting, `${APP}/${LOGIN}/MenuBar.app/Contents/Info.plist`]));
  assert.throws(() => run({}, fakeSpawn().spawn, baseExisting.filter((file) => !file.endsWith("Package.swift"))), { code: "NATIVE_HELPERS_INPUT_MISSING" });
  assert.throws(() => run({ helpers: [cloud()], sharedRoot: "/rk-rightkit" }, fakeSpawn().spawn, baseExisting.filter((file) => !file.endsWith("cloud.plist"))), { code: "NATIVE_HELPERS_INPUT_MISSING", message: /entitlements/ });
  assert.throws(() => run({ version: undefined }), { code: "NATIVE_HELPERS_INVALID" });
  assert.throws(() => run({}, fakeSpawn().spawn, baseExisting.filter((file) => !file.endsWith("Contents/Info.plist"))), { code: "NATIVE_HELPERS_INPUT_MISSING" });
});

test("App Intents metadata runs appintentsmetadataprocessor on the SwiftPM const-value inputs and requires the output", () => {
  const intermediates = "/rk-helpers/scratch/pkg/.build/apple/Intermediates.noindex";
  const sourceList = `${intermediates}/Pkg.build/Release/arm64/Shortcuts.SwiftFileList`;
  const output = `${APP}/${LOGIN}/Shortcuts.app/Contents/Resources/Metadata.appintents/extract.actionsdata`;
  const { fs, files } = memoryFs([...baseExisting, `${BIN}/Shortcuts`]);
  fs.readdirSync = (dir) => {
    const entry = (name, isDirectory) => ({ name, isDirectory: () => isDirectory, isFile: () => !isDirectory });
    if (dir === intermediates) return [entry("Pkg.build", true)];
    if (dir === `${intermediates}/Pkg.build`) return [entry("Release", true)];
    if (dir === `${intermediates}/Pkg.build/Release`) return [entry("arm64", true)];
    if (dir === `${intermediates}/Pkg.build/Release/arm64`) return [entry("Shortcuts.SwiftFileList", false), entry("b.swiftconstvalues", false), entry("a.swiftconstvalues", false)];
    throw new Error(`unexpected ${dir}`);
  };
  const calls = [];
  const spawn = (cmd, args, options) => {
    calls.push({ cmd, args, options });
    if (args.includes("--show-bin-path")) return { status: 0, stdout: `${BIN}\n` };
    if (cmd === "lipo") return { status: 0, stdout: "arm64 x86_64\n" };
    if (cmd === "xcrun" && args[0] === "--find") return { status: 0, stdout: "/Xcode/Toolchains/XcodeDefault.xctoolchain/usr/bin/swiftc\n" };
    if (cmd === "xcrun") return { status: 0, stdout: "/Xcode/SDKs/MacOSX.sdk\n" };
    if (cmd === "xcodebuild") return { status: 0, stdout: "Xcode 27.0\nBuild version 1\n" };
    if (cmd.endsWith("appintentsmetadataprocessor")) { files.set(output, Buffer.from("x")); return { status: 0 }; }
    return { status: 0, stdout: "" };
  };
  const helper = { name: "shortcuts", package: "macos-native/Pkg", product: "Shortcuts", kind: "app", embed: `${LOGIN}/Shortcuts.app`, universal: true, info: { bundleId: "com.example.shortcuts" }, appIntents: { moduleName: "Shortcuts", deploymentTarget: "14.0" } };
  buildAndEmbedNativeHelpers({ appRoot: ROOT, app: APP, helpers: [helper], version: "1.0.0", env: {}, spawn, fs });
  const processor = calls.find((call) => call.cmd.endsWith("appintentsmetadataprocessor"));
  assert.equal(processor.cmd, "/Xcode/Toolchains/XcodeDefault.xctoolchain/usr/bin/appintentsmetadataprocessor");
  for (const [flag, value] of [["--module-name", "Shortcuts"], ["--deployment-target", "14.0"], ["--target-triple", "arm64-apple-macosx14.0"], ["--source-file-list", sourceList], ["--xcode-version", "27.0"], ["--toolchain-dir", "/Xcode/Toolchains/XcodeDefault.xctoolchain"], ["--output", `${APP}/${LOGIN}/Shortcuts.app/Contents/Resources`]]) {
    assert.equal(processor.args[processor.args.indexOf(flag) + 1], value, flag);
  }
  // No inputs from SwiftPM: refused.
  fs.readdirSync = () => [];
  assert.throws(() => buildAndEmbedNativeHelpers({ appRoot: ROOT, app: APP, helpers: [helper], version: "1.0.0", env: {}, spawn, fs, replaceExisting: true }), { code: "NATIVE_HELPERS_APPINTENTS_FAILED" });
});

test("helpers are signed inside-out with their own entitlements and the hardened runtime, then the app, then verified", () => {
  const { fs } = memoryFs(baseExisting);
  const { spawn, calls } = fakeSpawn();
  const codesign = [];
  const signOne = (args) => { codesign.push(args); return { status: 0 }; };
  const inspect = (cmd, args) => (args[0] === "--verify" ? "valid on disk\nsatisfies its designated requirement" : "Authority=Developer ID Application: Demo\nTimestamp=now");
  const nestedInCloud = `${APP}/${LOGIN}/CloudHelper.app/Contents/Frameworks/Lib.framework`;
  const discover = (helperApp) => (helperApp.endsWith("CloudHelper.app") ? [nestedInCloud] : []);
  const order = [];
  const result = buildEmbedSignNativeHelpers({
    appRoot: ROOT, app: APP, helpers: [menu(), cloud(), tool()], version: "1.2.3", env: {}, fs, sharedRoot: "/rk-rightkit",
    spawn: (cmd, args, options) => { order.push(cmd === "codesign" ? "codesign-verify" : args.includes("--show-bin-path") ? "bin" : cmd); return spawn(cmd, args, options); },
    signing: { identity: "Developer ID Application: Demo", signOne: (args) => { order.push("sign"); return signOne(args); }, inspect, discover, evidence: () => ({}), hashFile: () => "h" },
    signApp: () => order.push("sign-app"),
  });
  assert.equal(order.filter((step) => step === "swift").length, 3);
  assert.ok(order.lastIndexOf("swift") < order.indexOf("sign"), "every helper is built and embedded before the first signature");
  assert.deepEqual(order.slice(-6), ["sign", "sign", "sign", "sign", "sign-app", "codesign-verify"]);
  // Deepest first: the code nested in a helper, then the helpers (deeper paths before shallower), then the executable beside the main one.
  const signed = result.events.filter((event) => event.startsWith("sign:")).slice(0, -1);
  assert.equal(signed[0], `sign:${path.relative(APP, nestedInCloud)}`);
  assert.deepEqual(signed.slice(1), [`sign:${LOGIN}/CloudHelper.app`, `sign:${LOGIN}/MenuBar.app`, "sign:Contents/MacOS/tool"].sort((a, b) => b.split("/").length - a.split("/").length || a.localeCompare(b)));
  assert.equal(result.events.at(-2), "sign:app");
  assert.equal(result.events.at(-1), "verify");
  const cloudSign = codesign.find((args) => args.includes(`${APP}/${LOGIN}/CloudHelper.app`));
  assert.equal(cloudSign[cloudSign.indexOf("--entitlements") + 1], `${ROOT}/src-tauri/cloud.plist`);
  assert.ok(cloudSign.includes("runtime") || cloudSign.join(" ").includes("--options"), "hardened runtime");
  const menuSign = codesign.find((args) => args.includes(`${APP}/${LOGIN}/MenuBar.app`));
  assert.ok(!menuSign.includes("--entitlements"), "no entitlements unless declared");
  assert.deepEqual(calls.filter((call) => call.cmd === "codesign").map((call) => call.args), [["--verify", "--strict", "--deep", APP]]);
  assert.throws(() => buildEmbedSignNativeHelpers({ appRoot: ROOT, app: APP, helpers: [menu()], version: "1.0.0" }), { code: "NATIVE_HELPERS_INVALID" });
});

test("a failed app verification is an error and signNativeHelpers needs an identity", () => {
  const { fs } = memoryFs(baseExisting);
  assert.throws(() => signNativeHelpers({ app: APP, appRoot: ROOT, helpers: [menu()], identity: "", signOne() {}, inspect() {} }), { code: "NATIVE_HELPERS_SIGN_FAILED" });
  const plan = planNativeHelperSigning({ app: APP, appRoot: ROOT, helpers: [menu(), tool()], discover: () => [] });
  assert.deepEqual(plan.map((entry) => path.relative(APP, entry.file)), [`${LOGIN}/MenuBar.app`, "Contents/MacOS/tool"]);
  assert.ok(plan.every((entry) => entry.hardenedRuntime === true));
  const spawn = (cmd, args) => (cmd === "codesign" ? { status: 1, stderr: "bad" } : fakeSpawn().spawn(cmd, args));
  const inspect = (cmd, args) => (args[0] === "--verify" ? "valid on disk\nsatisfies its designated requirement" : "Authority=ID\nTimestamp=now");
  assert.throws(() => buildEmbedSignNativeHelpers({ appRoot: ROOT, app: APP, helpers: [tool()], version: "1.0.0", env: {}, fs, spawn, signing: { identity: "ID", signOne: () => ({ status: 0 }), inspect, discover: () => [], evidence: () => ({}), hashFile: () => "h" }, signApp() {} }), { code: "NATIVE_HELPERS_VERIFY_FAILED" });
});

test("the release lane signs a helper .app after the code inside it and before the main app (planMacSigning over the derived nestedCode)", () => {
  const helpers = validateNativeHelpers([cloud()]);
  const nested = Object.entries(nativeHelperNestedCode(helpers)).map(([key, value]) => ({ key, options: [], ...(value.entitlements ? { entitlements: path.join(ROOT, value.entitlements) } : {}) }));
  const helperApp = `${APP}/${LOGIN}/CloudHelper.app`;
  const inner = `${helperApp}/Contents/MacOS/CloudHelper`;
  const plan = planMacSigning({
    entries: [{ file: APP, options: [] }], nestedCode: nested, root: ROOT,
    expand: () => ({ nested: [inner, helperApp] }), lstat: () => ({ isSymbolicLink: () => false }), realpath: (file) => file, mainName: () => "Demo",
  });
  const order = plan.nested.map((item) => item.file);
  assert.ok(order.indexOf(inner) < order.indexOf(helperApp), "executable inside the helper first");
  assert.equal(plan.nested.find((item) => item.file === helperApp).entitlements, `${ROOT}/src-tauri/cloud.plist`);
  assert.equal(plan.top[0].file, APP, "the app is signed last by release.mjs");
});
