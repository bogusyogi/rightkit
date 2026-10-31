import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import fs from "node:fs";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { BUILD_LAUNCH_REFUSED, buildLaunch, createNativeRunner } from "./build-launch.mjs";
import { managedRuntimeClient } from "./managed-roots.mjs";
import { LINUX_BROKER_CLIENT_ENV, LINUX_BROKER_CLIENT_SHA256_ENV, LINUX_BROKER_SOCKET_ENV } from "./managed-cargo.mjs";
import { NATIVE_SIGNING_FILE_ENV, nativeManagedOutputs, nativeOutputsFromReceipt, nativeRunToken, nativeStateDir, signingFileProblem } from "./native-adapters.mjs";

const CLIENT = "/managed/agent-bin/rustright";
const managed = { env: { PATH: "/managed/agent-bin" }, platform: "darwin", exists: (p) => p === CLIENT, which: (c) => (c === "rustright" ? [CLIENT] : []) };
const direct = { env: { PATH: "/usr/bin" }, platform: "linux", exists: () => false, which: () => [] };
// Broker-managed (a RustRight data root exists) but the client is unreachable from this shell.
const unreachable = { env: { PATH: "/usr/bin", RIGHTKIT_BUILD_BROKER_SOCKET: "/x/broker.sock" }, platform: "darwin", exists: () => true, which: () => [], readFile: () => "" };
const ownRightRelease = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "cli", "right-release.mjs");

test("managed host: pnpm is a rightkit-pnpm broker job; cargo and rustright go by the broker client; the door's own right-release is unchanged", () => {
  assert.deepEqual(buildLaunch("pnpm", ["--dir", "apps/web", "run", "build"], managed), { command: CLIENT, args: ["run", "rightkit-pnpm", "--", "--dir", "apps/web", "run", "build"] });
  assert.deepEqual(buildLaunch("pnpm.cmd", ["run", "test"], managed).args, ["run", "rightkit-pnpm", "--", "run", "test"]);
  assert.deepEqual(buildLaunch("cargo", ["build"], managed), { command: CLIENT, args: ["cargo", "build"] });
  assert.deepEqual(buildLaunch("swift", ["build", "--package-path", "swift/helper"], managed), { command: CLIENT, args: ["run", "rightkit-swift", "--", "build", "--package-path", "swift/helper"] });
  // A real cargo named by absolute path is still a broker job: the path is not trusted, the name is routed.
  assert.deepEqual(buildLaunch("/Users/x/.cargo/bin/cargo", ["build"], managed), { command: CLIENT, args: ["cargo", "build"] });
  assert.deepEqual(buildLaunch("/elsewhere/rustright", ["cargo", "test"], managed), { command: CLIENT, args: ["cargo", "test"] });
  assert.deepEqual(buildLaunch(process.execPath, [ownRightRelease, "web-build"], managed), { command: process.execPath, args: [ownRightRelease, "web-build"] });
});

test("managed host: node runs a workspace script as a rightkit-node broker job; a rightkit-qa run is a rightkit-qa broker job", () => {
  const host = { ...managed, cwd: "/w/app" };
  assert.deepEqual(buildLaunch("node", ["node_modules/vite/bin/vite.js", "build"], host), { command: CLIENT, args: ["run", "rightkit-node", "--", "node_modules/vite/bin/vite.js", "build"] });
  assert.deepEqual(buildLaunch(process.execPath, ["/w/app/scripts/dev-build.mjs", "--platform", "mac"], host).args, ["run", "rightkit-node", "--", "scripts/dev-build.mjs", "--platform", "mac"]);
  // GB-27: the broker registers no rightkit-qa adapter; outside a snapshot stage a QA step has no route (see the GB-27 test below).
  assert.throws(() => buildLaunch("rightkit-qa", ["run-tests", "--app", "heardright"], host), (e) => e.code === BUILD_LAUNCH_REFUSED && /no uncarried route/.test(e.message));
});

test("[GB-27] a rightkit-qa step with no snapshot carrier never requests the unregistered rightkit-qa adapter, in either spelling; it is refused before any spawn", () => {
  const host = { ...managed, cwd: "/w/app" };
  for (const [command, args] of [["rightkit-qa", ["run-tests", "--app", "a"]], ["rustright", ["run", "rightkit-qa", "--", "run-tests", "--app", "a"]]]) {
    let launched = null;
    try { launched = buildLaunch(command, args, host); } catch (e) { assert.equal(e.code, BUILD_LAUNCH_REFUSED, `${command}: ${e.message}`); continue; }
    assert.notDeepEqual(launched.args?.slice(0, 2), ["run", "rightkit-qa"], `${command} must not be sent as an adapter job: ${JSON.stringify(launched)}`);
    assert.fail(`${command} ${args.join(" ")} was launched without a carrier: ${JSON.stringify(launched)}`);
  }
});

test("managed host: any other build or test command is refused, never run around the broker", () => {
  // An executable merely named node that names right-release is not this Node binary running this package's right-release.
  assert.throws(() => buildLaunch("/tmp/evil/node", [ownRightRelease, "web-build"], { ...managed, cwd: "/w/app" }), (e) => e.code === BUILD_LAUNCH_REFUSED);
  for (const [command, args] of [["vite", ["build"]], ["npm", ["run", "build"]], ["sh", ["scripts/prepackage-mac.sh"]], ["xcodebuild", ["-showBuildSettings"]], ["gradle", ["publish"]], ["make", ["all"]], ["sh", ["-c", "pnpm run build"]], ["tsc", ["-p", "."]], ["node", ["/elsewhere/build.mjs"]], ["node", ["../other/build.mjs"]], ["node", ["-e", "1"]], ["powershell", ["-File", "x.ps1"]]]) {
    assert.throws(() => buildLaunch(command, args, managed), (e) => e.code === BUILD_LAUNCH_REFUSED && /rightkit#1241/.test(e.message), command);
  }
});

test("host detection reads the cargo on PATH: the RightKit cargo guard makes it managed (pnpm then needs the client); a plain cargo is direct", () => {
  // Mac and Windows only: every Linux host is direct whatever cargo is on PATH (gate-executor directPlatform).
  const guard = { env: { PATH: "/g" }, platform: "darwin", exists: () => false, which: (c) => (c === "cargo" ? ["/g/cargo"] : []), readFile: () => "#!/usr/bin/env node\nimport './cargo-guard.mjs';\n" };
  assert.throws(() => buildLaunch("pnpm", ["run", "build"], guard), (e) => e.code === BUILD_LAUNCH_REFUSED && /rustright is not on PATH/.test(e.message));
  const plain = { ...guard, readFile: () => "\x7fELF" };
  assert.deepEqual(buildLaunch("pnpm", ["run", "build"], plain), { command: "pnpm", args: ["run", "build"] });
  assert.equal(buildLaunch("pnpm", ["run", "build"], { ...guard, platform: "linux" }).command, "pnpm");
});

test("a managed host whose broker is unreachable refuses every launch; a host without a broker runs directly", () => {
  for (const [command, args] of [["pnpm", ["run", "build"]], ["cargo", ["build"]], ["node", ["scripts/build.mjs"]], ["rightkit-qa", ["run-tests"]]]) {
    assert.throws(() => buildLaunch(command, args, unreachable), (e) => e.code === BUILD_LAUNCH_REFUSED && /broker-managed/.test(e.message), command);
  }
  assert.deepEqual(buildLaunch("pnpm", ["run", "build"], direct), { command: "pnpm", args: ["run", "build"] });
  assert.deepEqual(buildLaunch("vite", ["build"], direct), { command: "vite", args: ["build"] });
});

test("managed host: node flags that change no code path pass to the adapter; eval, import, require and loader flags refuse", () => {
  const host = { ...managed, cwd: "/w/app" };
  assert.deepEqual(buildLaunch("node", ["--disable-warning=DEP0205", "node_modules/typescript/bin/tsc"], host).args, ["run", "rightkit-node", "--", "--disable-warning=DEP0205", "node_modules/typescript/bin/tsc"]);
  for (const flags of [["-e", "1"], ["--eval=1"], ["-p", "1"], ["--import", "x.mjs"], ["--import=x.mjs"], ["-r", "x"], ["--require=x"], ["--loader", "x"], ["--experimental-loader=x"]]) {
    assert.throws(() => buildLaunch("node", [...flags, "scripts/build.mjs"], host), (e) => e.code === BUILD_LAUNCH_REFUSED, flags.join(" "));
  }
});

test("managed host: npm (whose install lifecycle scripts can build) refuses; the door's own right-release refuses added startup code", () => {
  assert.throws(() => buildLaunch("npm", ["ci"], managed), (e) => e.code === BUILD_LAUNCH_REFUSED);
  const injected = { ...managed, env: { ...managed.env, NODE_OPTIONS: `${process.env.NODE_OPTIONS ?? ""} --import=/tmp/build.mjs`.trim() } };
  assert.throws(() => buildLaunch(process.execPath, [ownRightRelease, "web-build"], injected), (e) => e.code === BUILD_LAUNCH_REFUSED && /NODE_OPTIONS/.test(e.message));
  // Reordered tokens of an inherited value can re-pair an option with another value: anything but the exact string refuses.
  const original = process.env.NODE_OPTIONS;
  process.env.NODE_OPTIONS = "--max-old-space-size=4096 --import ./a.mjs";
  try {
    const reordered = { ...managed, env: { ...managed.env, NODE_OPTIONS: "--import --max-old-space-size=4096 ./a.mjs" } };
    assert.throws(() => buildLaunch(process.execPath, [ownRightRelease, "web-build"], reordered), (e) => e.code === BUILD_LAUNCH_REFUSED);
    const exact = { ...managed, env: { ...managed.env, NODE_OPTIONS: process.env.NODE_OPTIONS } };
    assert.equal(buildLaunch(process.execPath, [ownRightRelease, "web-build"], exact).command, process.execPath);
  } finally { if (original === undefined) delete process.env.NODE_OPTIONS; else process.env.NODE_OPTIONS = original; }
  const same = { ...managed, env: { ...managed.env, ...(process.env.NODE_OPTIONS !== undefined ? { NODE_OPTIONS: process.env.NODE_OPTIONS } : {}) } };
  assert.deepEqual(buildLaunch(process.execPath, [ownRightRelease, "web-build"], same).command, process.execPath);
});

// ---- GB-06: the helpers a package recipe generates are this package's own release operations ----
test("[GB-06] the exact installed recipe-build.mjs and create-mac-updater.mjs run unchanged on a broker host (own session, their builds come back through the door); any other script, a copy under an app, or a changed NODE_OPTIONS is refused", async () => {
  const { INTERNAL_RELEASE_HELPERS } = await import("./build-launch.mjs");
  const { fileURLToPath } = await import("node:url");
  const HERE_DIR = path.dirname(fileURLToPath(import.meta.url));
  assert.deepEqual(INTERNAL_RELEASE_HELPERS.map((f) => path.basename(f)), ["right-release.mjs", "recipe-build.mjs", "create-mac-updater.mjs"]);
  const broker = { env: { PATH: "/managed/agent-bin" }, platform: "darwin", exists: (p) => p === "/managed/agent-bin/rustright", which: () => ["/managed/agent-bin/rustright"], readFile: () => "", cwd: "/app" };
  for (const helper of [path.join(HERE_DIR, "recipe-build.mjs"), path.join(HERE_DIR, "create-mac-updater.mjs")]) {
    // The generated environment (the quiet Tauri signing variables) is the helper process's own, never an adapter request's.
    const launch = buildLaunch(process.execPath, [helper, "--plan", "{}"], { ...broker, stepEnv: { TAURI_SIGNING_PRIVATE_KEY: "", TAURI_SIGNING_PRIVATE_KEY_PASSWORD: "" } });
    assert.deepEqual(launch, { command: process.execPath, args: [helper, "--plan", "{}"] }, path.basename(helper));
  }
  // Not the installed helper: an app's copy, another absolute script, a relative one, a different executable named node.
  // An app's own copy is a workspace script like any other: it is carried as the rightkit-node ADAPTER job (never run unchanged, never in the gate's session).
  const copy = buildLaunch(process.execPath, ["/app/node_modules/@rightkit/release/recipe-build.mjs", "--plan", "{}"], broker);
  assert.deepEqual(copy.args.slice(0, 3), ["run", "rightkit-node", "--"], "the app's copy is an adapter job");
  for (const script of ["/tmp/evil/recipe-build.mjs", path.join(HERE_DIR, "..", "elsewhere", "recipe-build.mjs")]) {
    assert.throws(() => buildLaunch(process.execPath, [script, "--plan", "{}"], broker), (e) => e.code === BUILD_LAUNCH_REFUSED, script);
  }
  assert.throws(() => buildLaunch("/usr/local/bin/node", [path.join(HERE_DIR, "recipe-build.mjs")], broker), (e) => e.code === BUILD_LAUNCH_REFUSED, "an executable that merely is named node is not this process");
  // No added startup code: NODE_OPTIONS must be empty or exactly this process's own.
  assert.throws(() => buildLaunch(process.execPath, [path.join(HERE_DIR, "recipe-build.mjs")], { ...broker, env: { ...broker.env, NODE_OPTIONS: "--import /tmp/x.mjs" } }), (e) => e.code === BUILD_LAUNCH_REFUSED && /NODE_OPTIONS/.test(e.message));
});

// ---- RA-07, RA-08, RA-09: the native-tool door mirrors RustRight's typed grammar (rustright-policy native_adapters.rs, RR-NATIVE 96b6334) ----
// The fixtures are real directories (the Gradle wrapper's properties, export options files); the hosts are the door's own inputs (which, exists, readFile), the same description the other tests give it.
const MAC_ENV = { PATH: "/usr/bin", HOME: "/Users/rk" };
const MAC_CLIENT = managedRuntimeClient(MAC_ENV, "darwin", () => true);
const ALIEN_CLIENT = "/elsewhere/on/path/rustright";
// The PATH carries another `rustright`: the executor choice notices it (a broker host), but a native launch never picks it.
const macHost = (cwd, over = {}) => ({ env: MAC_ENV, platform: "darwin", exists: (p) => p === MAC_CLIENT, which: (c) => (c === "rustright" ? [ALIEN_CLIENT] : []), readFile: () => "", cwd, ...over });
const PLIST = (destination) => `<?xml version="1.0" encoding="UTF-8"?>\n<plist version="1.0"><dict><key>method</key><string>app-store-connect</string><key>destination</key>\n  <string>${destination}</string></dict></plist>\n`;

function nativeApp(t) {
  const dir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-native-door-")));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  for (const [sub, sum] of [["android", `distributionSha256Sum=${"a".repeat(64)}\n`], ["bare", ""]]) {
    fs.mkdirSync(path.join(dir, sub, "gradle", "wrapper"), { recursive: true });
    fs.writeFileSync(path.join(dir, sub, "gradle", "wrapper", "gradle-wrapper.properties"), `distributionUrl=https://services.gradle.org/distributions/gradle-8.9-bin.zip\n${sum}`);
  }
  fs.writeFileSync(path.join(dir, "ExportOptions.plist"), PLIST("export"));
  fs.writeFileSync(path.join(dir, "Upload.plist"), PLIST("upload"));
  fs.writeFileSync(path.join(dir, "Binary.plist"), "bplist00");
  return dir;
}

test("[RA-07/RA-09] what the broker's grammar accepts is sent as it is, by the executor's own client and never a PATH one; an absolute path the gate wrote under the step's directory is made relative", (t) => {
  const dir = nativeApp(t);
  const host = macHost(dir);
  const sent = (command, args, over = {}) => { const launch = buildLaunch(command, args, { ...host, ...over }); assert.equal(launch.command, MAC_CLIENT, `${command}: the managed runtime client, not ${ALIEN_CLIENT}`); return launch; };
  const xcode = (args) => sent("xcodebuild", args).args;
  assert.deepEqual(xcode(["archive", "-project", path.join(dir, "App.xcodeproj"), "-scheme", "App", "-configuration", "Release", "-destination", "generic/platform=iOS", "-allowProvisioningUpdates"]),
    ["run", "rightkit-xcodebuild", "--", "archive", "-project", "App.xcodeproj", "-scheme", "App", "-configuration", "Release", "-destination", "generic/platform=iOS", "-allowProvisioningUpdates"]);
  assert.deepEqual(xcode(["-exportArchive", "-exportOptionsPlist", path.join(dir, "ExportOptions.plist"), "-allowProvisioningUpdates"]), ["run", "rightkit-xcodebuild", "--", "-exportArchive", "-exportOptionsPlist", "ExportOptions.plist", "-allowProvisioningUpdates"]);
  assert.deepEqual(xcode(["test", "-project", "App.xcodeproj", "-scheme", "AppUITests", "-testPlan", "UITests", "-only-testing:AppUITests/LoginTests/testLogin()", "-skip-testing:AppUITests"]),
    ["run", "rightkit-xcodebuild", "--", "test", "-project", "App.xcodeproj", "-scheme", "AppUITests", "-testPlan", "UITests", "-only-testing:AppUITests/LoginTests/testLogin()", "-skip-testing:AppUITests"]);
  assert.deepEqual(xcode(["build-for-testing", "-scheme", "App", "-only-testing:AppTests"]).slice(3), ["build-for-testing", "-scheme", "App", "-only-testing:AppTests"]);
  assert.deepEqual(sent("xcodegen", ["generate", "--spec", path.join(dir, "project.yml"), "--project", dir, "--use-cache"]).args, ["run", "rightkit-xcodegen", "--", "generate", "--spec", "project.yml", "--project", ".", "--use-cache"]);
  // CMake: a configure, a build and an install are three shapes; the mode flag is first and no directory is ever named.
  assert.deepEqual(sent("cmake", ["-S", path.join(dir, "src"), "-G", "Ninja", "-DCMAKE_BUILD_TYPE=Release"]).args, ["run", "rightkit-cmake", "--", "-S", "src", "-G", "Ninja", "-DCMAKE_BUILD_TYPE=Release"]);
  assert.deepEqual(sent("cmake", ["--build", "--config", "Release", "--target", "app", "--parallel"]).args.slice(3), ["--build", "--config", "Release", "--target", "app", "--parallel"]);
  assert.deepEqual(sent("cmake", ["--install", "--config", "Release"]).args.slice(3), ["--install", "--config", "Release"]);
  // Gradle: from the wrapper's own directory, or `-p` the gate wrote as an absolute path under the step's directory.
  assert.deepEqual(sent("./gradlew", [":app:assembleDebug", "--offline", "--no-daemon"], { cwd: path.join(dir, "android") }).args, ["run", "rightkit-gradle", "--", ":app:assembleDebug", "--offline", "--no-daemon"]);
  assert.deepEqual(sent("./gradlew", ["-p", path.join(dir, "android"), ":app:assembleDebug"]).args, ["run", "rightkit-gradle", "--", "-p", "android", ":app:assembleDebug"]);
  assert.deepEqual(sent("./gradlew", [`--project-dir=${path.join(dir, "android")}`, ":app:assembleDebug"]).args, ["run", "rightkit-gradle", "--", "--project-dir=android", ":app:assembleDebug"]);
  // The launch names what the receipt mapping needs: the tool, the adapter, the argv as sent and no signing material.
  const launch = sent("xcodebuild", ["archive", "-scheme", "App"]);
  assert.deepEqual({ ...launch.native, relativeCwd: undefined }, { tool: "xcodebuild", adapter: "rightkit-xcodebuild", capability: "adapter:rightkit-xcodebuild", args: ["archive", "-scheme", "App"], relativeCwd: undefined, carried: false });
  assert.equal(launch.env, undefined, "a native step carries no environment");
});

test("[RA-07/RA-09] what the broker's grammar or its shape rules refuse is refused here, typed and naming the capability, before anything is sent", (t) => {
  const dir = nativeApp(t);
  const cases = [
    // output ownership: a caller never names where a build writes
    ["xcodebuild", ["build", "-derivedDataPath", "/x"], dir, "RK_NATIVE_TOOL_REFUSED", /names where the build writes/],
    ["xcodebuild", ["archive", "-archivePath", "a.xcarchive"], dir, "RK_NATIVE_TOOL_REFUSED", /-archivePath names where the build writes/],
    ["xcodebuild", ["test", "-resultBundlePath", "r.xcresult"], dir, "RK_NATIVE_TOOL_REFUSED", /-resultBundlePath names where/],
    ["xcodebuild", ["-exportArchive", "-exportPath", "e", "-exportOptionsPlist", "ExportOptions.plist"], dir, "RK_NATIVE_TOOL_REFUSED", /-exportPath names where/],
    ["cmake", ["-S", ".", "-B", "build"], dir, "RK_NATIVE_TOOL_REFUSED", /-B names where the build writes/],
    ["cmake", ["-S", ".", "-Bbuild"], dir, "RK_NATIVE_TOOL_REFUSED", /names where the build writes/],
    ["cmake", ["--build", "build"], dir, "RK_NATIVE_TOOL_REFUSED", /positional argument "build" is not allowed/],
    ["cmake", ["--install", "out", "--config", "Release"], dir, "RK_NATIVE_TOOL_REFUSED", /positional argument "out"/],
    ["cmake", ["-S", ".", "--install-prefix", "/opt/x"], dir, "RK_NATIVE_TOOL_REFUSED", /--install-prefix names where/],
    ["cmake", ["-S", ".", "-DCMAKE_INSTALL_PREFIX=/opt/x"], dir, "RK_NATIVE_TOOL_REFUSED", /-DCMAKE_INSTALL_PREFIX moves where CMake writes/],
    ["cmake", ["-S", ".", "-DCMAKE_RUNTIME_OUTPUT_DIRECTORY_RELEASE:PATH=out"], dir, "RK_NATIVE_TOOL_REFUSED", /moves where CMake writes/],
    ["cmake", ["-S", ".", "-Dfetchcontent_base_dir=/x"], dir, "RK_NATIVE_TOOL_REFUSED", /moves where CMake writes/],
    ["cmake", ["--config", "Release", "--build"], dir, "RK_NATIVE_TOOL_REFUSED", /--build must be the first argument/],
    ["cmake", ["--build", "--install"], dir, "RK_NATIVE_TOOL_REFUSED", /--build or --install, not both/],
    ["cmake", ["-S", ".", "--config", "Release"], dir, "RK_NATIVE_TOOL_REFUSED", /--config belongs to --build or --install/],
    ["cmake", ["--build", "-DX=1"], dir, "RK_NATIVE_TOOL_REFUSED", /-D configures/],
    ["cmake", ["-S", ".", "-DCMAKE_COMMAND=/tmp/evil"], dir, "RK_NATIVE_TOOL_REFUSED", /never admits|wrong kind/],
    ["gradle", ["--gradle-user-home", "/x", ":a:b"], path.join(dir, "android"), "RK_NATIVE_TOOL_REFUSED", /--gradle-user-home names where/],
    ["gradle", ["--project-cache-dir=/x", ":a:b"], path.join(dir, "android"), "RK_NATIVE_TOOL_REFUSED", /--project-cache-dir names where/],
    ["gradle", ["--init-script", "x.gradle", ":a:b"], path.join(dir, "android"), "RK_NATIVE_TOOL_REFUSED", /--init-script names where/],
    ["gradle", ["-I", "x.gradle", ":a:b"], path.join(dir, "android"), "RK_NATIVE_TOOL_REFUSED", /-I names where/],
    ["gradle", ["--include-build", "../x", ":a:b"], path.join(dir, "android"), "RK_NATIVE_TOOL_REFUSED", /--include-build names where/],
    ["gradle", [":app:bundleRelease"], path.join(dir, "android"), "RK_NATIVE_TOOL_REFUSED", /positional argument ":app:bundleRelease" is not allowed/],
    ["gradle", ["publish"], path.join(dir, "android"), "RK_NATIVE_TOOL_REFUSED", /not allowed/],
    ["xcodegen", ["generate", "--cache-path", "/x"], dir, "RK_NATIVE_TOOL_REFUSED", /--cache-path names where/],
    // the shapes the grammar cannot express
    ["xcodebuild", ["archive", "test"], dir, "RK_NATIVE_TOOL_REFUSED", /archive is the only action/],
    ["xcodebuild", ["-exportArchive", "build", "-exportOptionsPlist", "ExportOptions.plist"], dir, "RK_NATIVE_TOOL_REFUSED", /takes no action or build setting/],
    ["xcodebuild", ["-exportArchive"], dir, "RK_NATIVE_TOOL_REFUSED", /needs -exportOptionsPlist/],
    ["xcodebuild", ["-exportArchive", "-scheme", "S", "-exportOptionsPlist", "ExportOptions.plist"], dir, "RK_NATIVE_TOOL_REFUSED", /belongs to a build/],
    ["xcodebuild", ["-exportArchive", "-testPlan", "P", "-exportOptionsPlist", "ExportOptions.plist"], dir, "RK_NATIVE_TOOL_REFUSED", /does not belong to -exportArchive/],
    ["xcodebuild", ["build", "-exportOptionsPlist", "ExportOptions.plist"], dir, "RK_NATIVE_TOOL_REFUSED", /belongs to -exportArchive/],
    ["xcodebuild", ["build", "-testPlan", "P"], dir, "RK_NATIVE_TOOL_REFUSED", /needs build-for-testing, test or test-without-building/],
    ["xcodebuild", ["archive", "-only-testing:AppTests"], dir, "RK_NATIVE_TOOL_REFUSED", /needs build-for-testing, test or test-without-building/],
    ["xcodebuild", ["test", "-only-testing:../AppTests"], dir, "RK_NATIVE_TOOL_REFUSED", /is not a test identifier/],
    ["xcodebuild", ["test", "-only-testing:A/B/C/D"], dir, "RK_NATIVE_TOOL_REFUSED", /is not a test identifier/],
    ["xcodebuild", ["test", "-skip-testing:A//c"], dir, "RK_NATIVE_TOOL_REFUSED", /is not a test identifier/],
    ["xcodebuild", ["-exportArchive", "-exportOptionsPlist", "/etc/outside.plist"], dir, "RK_NATIVE_TOOL_REFUSED", /is outside the step's directory/],
    ["xcodebuild", ["-exportArchive", "-exportOptionsPlist", "a/../b.plist"], dir, "RK_NATIVE_TOOL_REFUSED", /wrong kind|relative/],
    ["xcodebuild", ["build", "CODE_SIGN_IDENTITY=Apple Development"], dir, "RK_NATIVE_TOOL_REFUSED", /not allowed/],
    ["xcodebuild", ["build", "SYMROOT=/tmp/out"], dir, "RK_NATIVE_TOOL_REFUSED", /not allowed/],
    ["xcodebuild", ["-showBuildSettings"], dir, "RK_NATIVE_TOOL_REFUSED", /not registered/],
    // what only the file system says
    ["gradle", ["--offline", ":app:assembleDebug"], path.join(dir, "bare"), "RK_ADAPTER_PIN_REQUIRED", /carries no distributionSha256Sum/],
    ["gradle", [":app:assembleDebug"], dir, "RK_ADAPTER_PIN_REQUIRED", /has no pinned identity/],
    ["gradle", ["-p", "bare", ":app:assembleDebug"], dir, "RK_ADAPTER_PIN_REQUIRED", /carries no distributionSha256Sum/],
    ["xcodebuild", ["-exportArchive", "-exportOptionsPlist", "Upload.plist"], dir, "RK_ADAPTER_ARGV_DENIED", /asks the export to upload/],
    ["xcodebuild", ["-exportArchive", "-exportOptionsPlist", "Binary.plist"], dir, "RK_ADAPTER_ARGV_DENIED", /must be an XML property list/],
    ["xcodebuild", ["-exportArchive", "-exportOptionsPlist", "Missing.plist"], dir, "RK_ADAPTER_ARGV_DENIED", /is not in the snapshot/],
  ];
  const capability = { xcodebuild: "adapter:rightkit-xcodebuild", cmake: "adapter:rightkit-cmake", gradle: "adapter:rightkit-gradle", xcodegen: "adapter:rightkit-xcodegen" };
  for (const [tool, args, cwd, reason, message] of cases) {
    assert.throws(() => buildLaunch(tool === "gradle" ? "./gradlew" : tool, args, macHost(cwd)), (e) => e.code === BUILD_LAUNCH_REFUSED && e.reason === reason && message.test(e.message) && e.message.includes(capability[tool]), `${tool} ${args.join(" ")}`);
  }
});

/** Unix socket paths are limited to 103 bytes on macOS and a hosted runner's hermetic temp root leaves no room: a socket's directory falls back to /tmp when the socket's path would not fit under the temp directory. */
const socketBase = (prefix, file) => (process.platform === "win32" || Buffer.byteLength(path.join(os.tmpdir(), `${prefix}XXXXXX`, file)) < 100 ? os.tmpdir() : "/tmp");
/** Windows cannot hold the declared Linux broker route's endpoint: a unix socket file on a POSIX path (the route is Linux's only, and its judging reads POSIX paths). */
const NO_UNIX_SOCKET = process.platform === "win32" && "a declared Linux broker names a unix socket file on a POSIX path, which Windows cannot hold";
test("[RA-07/RA-09] the native client is the resolved executor's: the installed managed runtime client; a PATH `rustright` is never launched", (t) => {
  const dir = nativeApp(t);
  // A Mac or Dell host whose installed runtime client is not there: the `rustright` on PATH is not an identity.
  assert.throws(() => buildLaunch("cmake", ["-S", "."], macHost(dir, { exists: () => false })), (e) => e.code === BUILD_LAUNCH_REFUSED && e.reason === "RK_NATIVE_CLIENT_UNRESOLVED" && /never launched through a PATH lookup/.test(e.message));
});

test("[RA-07/RA-09] the native client is the resolved executor's: the declared Linux broker's validated client wins over a PATH `rustright`, and an undeclared Linux host is direct", { skip: NO_UNIX_SOCKET }, async (t) => {
  const dir = nativeApp(t);
  // A declared Linux broker (a real client file and a real socket, as the route is judged): the validated client wins over a `rustright` first on PATH.
  const home = fs.realpathSync(fs.mkdtempSync(path.join(socketBase("rk-native-linux-", "broker.sock"), "rk-native-linux-")));
  const server = net.createServer();
  const socket = path.join(home, "broker.sock");
  await new Promise((resolve, reject) => { server.once("error", reject); server.listen(socket, resolve); });
  t.after(() => { server.close(); fs.rmSync(home, { recursive: true, force: true }); });
  const client = path.join(home, "rustright");
  const body = "#!/bin/sh\nexit 0\n";
  fs.writeFileSync(client, body); fs.chmodSync(client, 0o755);
  const declared = { PATH: "/usr/bin", [LINUX_BROKER_CLIENT_ENV]: client, [LINUX_BROKER_SOCKET_ENV]: socket, [LINUX_BROKER_CLIENT_SHA256_ENV]: createHash("sha256").update(body).digest("hex") };
  const linux = { env: declared, platform: "linux", exists: () => false, which: (c) => (c === "rustright" ? [ALIEN_CLIENT] : []), readFile: () => "", cwd: dir };
  const launch = buildLaunch("cmake", ["-S", "."], linux);
  assert.equal(launch.command, client);
  assert.deepEqual(launch.args, ["run", "rightkit-cmake", "--", "-S", "."]);
  assert.equal(launch.native.carried, false);
  assert.equal(launch.env?.RUSTRIGHT_BUILD_BROKER_SOCKET, socket, "the client talks to the endpoint the route validated");
  // An undeclared Linux host is direct: the real tool, as before (the broker's grammar applies only to a broker route), and no broker client.
  const plain = { env: { PATH: "/usr/bin" }, platform: "linux", exists: () => false, which: () => [], readFile: () => "", cwd: dir };
  assert.deepEqual(buildLaunch("cmake", ["-S", ".", "-B", "build"], plain), { command: "cmake", args: ["-S", ".", "-B", "build"] });
});

test("[RA-08] the signing-properties file is a typed path handoff: it never rides an environment or an argument; a broker route refuses it before launch, a direct host's Gradle receives the path only", (t) => {
  const dir = nativeApp(t);
  const cwd = path.join(dir, "android");
  const file = "/private/rk-android-sign-x/signing.properties";
  // A broker route (Mac, Dell, a declared Linux broker) has no request field that carries the file: refused before any launch, the path not echoed, nothing in an environment.
  let refusal = null;
  try { buildLaunch("./gradlew", [":app:assembleDebug"], { ...macHost(cwd), signingFile: file }); } catch (error) { refusal = error; }
  assert.equal(refusal?.code, BUILD_LAUNCH_REFUSED);
  assert.equal(refusal.reason, "RK_NATIVE_SIGNING_FILE_UNSUPPORTED");
  assert.match(refusal.message, /native-signing-file-v1/);
  assert.ok(!refusal.message.includes(file), "the refusal does not echo the path");
  // The same step with no file is carried as before; the environment variable never reaches the client of a launch that has none.
  const clean = buildLaunch("./gradlew", [":app:assembleDebug"], macHost(cwd));
  assert.equal(clean.env, undefined);
  // An environment that merely contains the variable is not a handoff: only `host.signingFile` is, and a step may not declare it.
  assert.throws(() => buildLaunch("./gradlew", [":app:assembleDebug"], macHost(cwd, { stepEnv: { [NATIVE_SIGNING_FILE_ENV]: file } })), (e) => e.code === BUILD_LAUNCH_REFUSED && e.reason === "RK_ADAPTER_ENV_SIGNING_FILE");
  // A direct host has no broker: its own Gradle gets the path (never the passwords) as it always did.
  const direct = { env: { PATH: "/usr/bin" }, platform: "linux", exists: () => false, which: () => [], readFile: () => "", cwd };
  assert.deepEqual(buildLaunch("./gradlew", [":app:bundleRelease"], { ...direct, signingFile: file }), { command: "./gradlew", args: [":app:bundleRelease"], env: { [NATIVE_SIGNING_FILE_ENV]: file } });
  assert.deepEqual(buildLaunch("./gradlew", [":app:bundleRelease"], direct), { command: "./gradlew", args: [":app:bundleRelease"] });
  // Another tool on a direct host never gets the variable.
  assert.deepEqual(buildLaunch("xcodebuild", ["build"], { ...direct, signingFile: file }), { command: "xcodebuild", args: ["build"], env: { [NATIVE_SIGNING_FILE_ENV]: file } });
});

test("[RA-08] the signing file the broker would lease is judged as the broker judges it: owner-only regular file, plain name, small, in a private directory inside the signer root", { skip: process.platform === "win32" ? "POSIX modes" : false }, (t) => {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-signer-root-")));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const dir = path.join(root, "rk-android-sign-1");
  fs.mkdirSync(dir, { mode: 0o700 });
  const file = path.join(dir, "signing.properties");
  fs.writeFileSync(file, "storeFile=x\n", { mode: 0o600 });
  const problem = (f, o = {}) => signingFileProblem(f, { signerRoot: root, ...o });
  assert.equal(problem(file), null);
  assert.match(problem("signing.properties"), /absolute path/);
  assert.match(problem(`${dir}/../x`), /absolute path without/);
  assert.match(problem(`${dir}/./signing.properties`), /absolute path without/);
  assert.match(problem(path.join(dir, ".hidden")), /not starting with '\.'/);
  assert.match(problem(path.join(dir, "bad name.properties")), /must be 1 to 128/);
  assert.match(problem(path.join(dir, "missing.properties")), /is not readable/);
  fs.chmodSync(file, 0o644);
  assert.match(problem(file), /owner only/);
  fs.chmodSync(file, 0o600);
  fs.chmodSync(dir, 0o777);
  assert.match(problem(file), /writable by others/);
  fs.chmodSync(dir, 0o700);
  fs.writeFileSync(path.join(dir, "empty.properties"), "", { mode: 0o600 });
  assert.match(problem(path.join(dir, "empty.properties")), /must hold 1 to/);
  fs.symlinkSync(file, path.join(dir, "link.properties"));
  assert.match(problem(path.join(dir, "link.properties")), /not a regular file/);
  const outside = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-signer-outside-")));
  t.after(() => fs.rmSync(outside, { recursive: true, force: true }));
  fs.chmodSync(outside, 0o700);
  fs.writeFileSync(path.join(outside, "signing.properties"), "x", { mode: 0o600 });
  assert.match(problem(path.join(outside, "signing.properties")), /not inside the signer root/);
  assert.match(problem(file, { signerRoot: path.join(root, "nope") }), /signer root .* cannot be resolved/);
  // The refusal on a broker route reports an unacceptable file too, without ever sending it.
  assert.throws(() => buildLaunch("./gradlew", [":app:assembleDebug"], { ...macHost(nativeApp(t)), signingFile: path.join(outside, "signing.properties"), signerRoot: root, cwd: path.join(nativeApp(t), "android") }), (e) => e.reason === "RK_NATIVE_SIGNING_FILE_UNSUPPORTED" && /not acceptable/.test(e.message));
});

test("[RA-07/RA-09] the receipt's output mapping: roles and paths under the job's managed state, exactly as the broker's managed call names them, read back and never guessed", () => {
  const outputDir = "/managed/ws/abc/out";
  const state = (tool, cwd = "") => nativeStateDir(outputDir, tool, cwd);
  assert.equal(state("xcodebuild", "ios"), path.join(outputDir, "native", "xcodebuild", createHash("sha256").update("ios").digest("hex")));
  assert.equal(state("cmake", ""), path.join(outputDir, "native", "cmake", createHash("sha256").update("").digest("hex")));
  assert.equal(state("cmake", "./"), state("cmake", ""), "the step directory is keyed by its snapshot-relative components");
  assert.equal(state("gradle", "a\\b"), state("gradle", "a/b"));
  const at = (tool, leaf, cwd = "ios") => path.join(state(tool, cwd), leaf);
  const outputs = (tool, args, over = {}) => nativeManagedOutputs(tool, args, { state: state(tool, "ios"), ...over });
  assert.deepEqual(outputs("xcodebuild", ["build", "-scheme", "A"]).outputs, [{ role: "derived-data", path: at("xcodebuild", "derived-data") }]);
  assert.deepEqual(outputs("xcodebuild", ["archive", "-scheme", "A"]).outputs, [{ role: "derived-data", path: at("xcodebuild", "derived-data") }, { role: "archive", path: at("xcodebuild", "archive/App.xcarchive") }]);
  assert.deepEqual(outputs("xcodebuild", ["-exportArchive", "-exportOptionsPlist", "o.plist"]).outputs, [{ role: "archive", path: at("xcodebuild", "archive/App.xcarchive") }, { role: "export", path: at("xcodebuild", "export") }]);
  assert.deepEqual(outputs("xcodebuild", ["test", "-scheme", "A"], { jobId: "job-7" }).outputs.map((o) => [o.role, o.path]), [["derived-data", at("xcodebuild", "derived-data")], ["result-bundle", at("xcodebuild", "results/job-7.xcresult")]]);
  assert.deepEqual(outputs("xcodebuild", ["build-for-testing", "-scheme", "A"]).outputs.at(-1), { role: "result-bundle", path: null, unresolved: true }, "the run-unique bundle needs the job id");
  assert.equal(nativeRunToken("job-7"), "job-7");
  assert.match(nativeRunToken("job 7/x"), /^j[0-9a-f]{32}$/);
  assert.equal(nativeRunToken("x".repeat(65)), `j${createHash("sha256").update("x".repeat(65)).digest("hex").slice(0, 32)}`);
  assert.deepEqual(outputs("xcodegen", ["generate"]).outputs, []);
  assert.deepEqual(outputs("xcodegen", ["generate", "--use-cache"]).outputs, [{ role: "cache", path: at("xcodegen", "cache") }]);
  assert.deepEqual(outputs("gradle", [":app:assembleDebug"]).outputs.map((o) => o.role), ["gradle-user-home", "project-cache", "build-tree", "init-script"]);
  assert.deepEqual(outputs("cmake", ["-S", "."]).outputs.map((o) => o.role), ["build-tree", "install-tree"]);
  assert.deepEqual(outputs("cmake", ["--build", "--config", "Release"]).outputs.map((o) => o.role), ["build-tree"]);
  assert.deepEqual(outputs("cmake", ["--install"]).outputs.map((o) => o.role), ["build-tree", "install-tree"]);
  assert.match(outputs("cmake", ["--config", "Release", "--build"]).problem ?? "", /must be the first argument/, "a call with no managed form maps nothing");
  // Read back from the signed receipt's `executable.managedOutputs`: accepted only when it is exactly what the sent args map to.
  const args = ["test", "-scheme", "A"];
  const managed = { tool: "xcodebuild", state: state("xcodebuild", "ios"), outputs: outputs("xcodebuild", args, { jobId: "job-7" }).outputs };
  const base = { tool: "xcodebuild", args, outputDir, relativeCwd: "ios", jobId: "job-7" };
  const read = nativeOutputsFromReceipt(managed, base);
  assert.equal(read.ok, true);
  assert.deepEqual(read.byRole, { "derived-data": at("xcodebuild", "derived-data"), "result-bundle": at("xcodebuild", "results/job-7.xcresult") });
  // Without the job id the run-unique bundle is accepted only under the state's results directory.
  assert.equal(nativeOutputsFromReceipt(managed, { ...base, jobId: undefined }).ok, true);
  const refused = (over, pattern, given = base) => { const r = nativeOutputsFromReceipt({ ...managed, ...over }, given); assert.equal(r.ok, false); assert.match(r.problem, pattern); };
  refused({ state: path.join(outputDir, "native", "xcodebuild", "0".repeat(64)) }, /is not the job's managed state/);
  refused({ tool: "gradle" }, /not xcodebuild/);
  refused({ outputs: managed.outputs.slice(0, 1) }, /maps 1 output/);
  refused({ outputs: [...managed.outputs].reverse() }, /is role/);
  refused({ outputs: [managed.outputs[0], { role: "result-bundle", path: "/elsewhere/r.xcresult" }] }, /expected|not under/);
  refused({ outputs: [{ ...managed.outputs[0], path: "/elsewhere/dd" }, managed.outputs[1]] }, /expected/);
  refused({ outputs: [{ role: "derived-data" }, managed.outputs[1]] }, /has no path/);
  assert.equal(nativeOutputsFromReceipt(undefined, base).ok, false);
  assert.equal(nativeOutputsFromReceipt(managed, { ...base, outputDir: "" }).ok, false);
});

test("[RA-07/RA-08/RA-09] the production runner: a refusal is a typed result before any launch with a fixed shape; a command that is not a native build tool has no generic spawn; the plan is the door's launch", (t) => {
  const dir = nativeApp(t);
  const runner = createNativeRunner({ cwd: dir, env: MAC_ENV, platform: "darwin", exists: (p) => p === MAC_CLIENT, which: (c) => (c === "rustright" ? [ALIEN_CLIENT] : []), readFile: () => "" });
  const SHAPE = ["adapter", "argv", "capability", "carried", "error", "nestedChildren", "ok", "outputDir", "outputs", "refusal", "relativeCwd", "requestId", "signal", "status", "stderr", "stdout", "tool"];
  for (const [command, args, code] of [
    ["xcodebuild", ["archive", "-derivedDataPath", "/x"], "RK_NATIVE_TOOL_REFUSED"],
    ["cmake", ["-S", ".", "-B", "build"], "RK_NATIVE_TOOL_REFUSED"],
    ["xcodebuild", ["-exportArchive", "-exportOptionsPlist", "Upload.plist"], "RK_ADAPTER_ARGV_DENIED"],
    ["pnpm", ["run", "build"], "RK_NATIVE_TOOL_REFUSED"],
    ["sh", ["-c", "xcodebuild archive"], "RK_NATIVE_TOOL_REFUSED"],
  ]) {
    const result = runner.run(command, args);
    assert.deepEqual(Object.keys(result).sort(), SHAPE, command);
    assert.equal(result.ok, false); assert.equal(result.status, null); assert.equal(result.refusal.code, code, `${command} ${args.join(" ")}`);
    assert.equal(result.error.code, BUILD_LAUNCH_REFUSED); assert.equal(result.outputs, null); assert.equal(result.argv, null);
  }
  const signed = runner.run("./gradlew", [":app:assembleDebug"], { cwd: path.join(dir, "android"), signingFile: "/private/x/signing.properties" });
  assert.equal(signed.refusal.code, "RK_NATIVE_SIGNING_FILE_UNSUPPORTED");
  assert.equal(signed.tool, "gradle"); assert.equal(signed.adapter, "rightkit-gradle"); assert.equal(signed.capability, "adapter:rightkit-gradle");
  const missing = createNativeRunner({ cwd: dir, env: MAC_ENV, platform: "darwin", exists: (p) => p === MAC_CLIENT, which: () => [ALIEN_CLIENT], readFile: () => "", brokerCapabilities: ["adapter:rightkit-gradle"] });
  assert.equal(missing.run("cmake", ["-S", "."]).refusal.code, "RK_NATIVE_CAPABILITY_MISSING");
  assert.match(missing.run("cmake", ["-S", "."]).refusal.message, /does not advertise adapter:rightkit-cmake/);
  const plan = runner.plan("xcodebuild", ["archive", "-scheme", "App"]);
  assert.deepEqual([plan.command, plan.args], [MAC_CLIENT, ["run", "rightkit-xcodebuild", "--", "archive", "-scheme", "App"]]);
  assert.throws(() => runner.plan("pnpm", ["run", "build"]), (e) => e.code === BUILD_LAUNCH_REFUSED && e.reason === "RK_NATIVE_TOOL_REFUSED");
});

test("[R3-X02] a cargo ndk output the broker refuses on its text (empty, the directory itself, inside .git) is refused before the send; the generated jniLibs directory goes through", (t) => {
  const host = macHost(nativeApp(t), { exists: (p) => p === MAC_CLIENT || p === "/usr/bin/rustright" });
  const ndk = (...out) => ["ndk", "-t", "arm64-v8a", "--platform", "28", ...out, "build", "--locked"];
  for (const out of [["-o", ""], ["-o", "."], ["-o", "./"], ["-o", ".git/jniLibs"], ["-o", "a/.GIT/x"], ["--output-dir", ".git"], ["--output-dir=android/.git/jniLibs"]]) {
    assert.throws(() => buildLaunch("cargo", ndk(...out), host), (e) => e.code === BUILD_LAUNCH_REFUSED && e.reason === "RK_NDK_OUTPUT_DENIED", out.join(" "));
  }
  assert.deepEqual(buildLaunch("cargo", ndk("-o", "android/app/src/main/jniLibs"), host).args, ["cargo", ...ndk("-o", "android/app/src/main/jniLibs")]);
  assert.deepEqual(buildLaunch("cargo", ["build", "-o", ".git"], host).args, ["cargo", "build", "-o", ".git"], "only cargo ndk's -o is judged");
});
