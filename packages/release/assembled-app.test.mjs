import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";

import {
  AssembledAppError, assembleMacBundle, assembleWindowsPayload, loadStageReceipt, resolveAssembly, validateMacAssembly, validateWindowsAssembly,
  verifyMacBundle, verifyReleaseBinding, verifyWindowsPayload, writeReleaseBinding, resolveNestedApp,
} from "./assembled-app.mjs";
import { assembleApp, nsisArguments, parseAssembleArgs } from "./assemble-app-command.mjs";
import { validateDevelopmentConfig } from "./development.mjs";

const sha = (text) => createHash("sha256").update(text).digest("hex");
const TRIPLE = "aarch64-apple-darwin";
const WIN_TRIPLE = "x86_64-pc-windows-msvc";

function fixture(t) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "rightkit-assembled-app-test-"));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const put = (relative, content = "x") => {
    const file = path.join(root, ...relative.split("/"));
    fs.mkdirSync(path.dirname(file), { recursive: true });
    fs.writeFileSync(file, content);
    return file;
  };
  const receipt = (rows, target = TRIPLE) => {
    const files = rows.map(({ name, kind = "cargo-bin", profile = "dev", content }) => {
      put(`dist/stage/${name}`, content);
      return { kind, name, ...(kind === "cargo-bin" ? { profile } : {}), path: `dist/stage/${name}`, sha256: sha(content), sizeBytes: content.length };
    });
    put("dist/stage/stage-sidecar.json", JSON.stringify({ schema: 1, contract: "rightkit-stage-sidecar-v1", target, files }));
  };
  return { root, put, receipt };
}

const macSpec = (extra = {}) => ({
  productName: "Demo", bundleId: "com.example.demo", targetTriple: TRIPLE,
  outer: { tool: "xcodebuild", projectSpec: "mac/project.yml", project: "mac/Demo.xcodeproj", scheme: "Demo" },
  nested: [{ built: "tauri", bundleDirectory: "hub/bundle", productName: "DemoHub", to: "Contents/Helpers/DemoHub.app" }],
  helpers: [{ product: "DemoHelper" }, { stage: "democli" }, { stage: "smartctl", kind: "remote" }, { stage: "ffmpeg", kind: "remote", optional: true, to: "Contents/Helpers/DemoHub.app/Contents/MacOS/ffmpeg" }],
  plugins: ["DemoFinder.appex"],
  launchDaemons: [{ from: "mac/helper.plist" }], launchAgents: [{ from: "mac/engine.plist" }],
  resources: [{ from: "third_party/lic", to: "Contents/Resources/lic" }, { from: "third_party/ffmpeg", to: "Contents/Resources/ffmpeg", ifStaged: "ffmpeg" }],
  bundleVersion: "commit-count",
  require: ["Contents/MacOS/Demo", "Contents/Helpers/democli", "Contents/Helpers/DemoHelper", "Contents/Helpers/DemoHub.app/Contents/Info.plist", "Contents/Library/LaunchDaemons/helper.plist", "Contents/Resources/lic/README"],
  devMarker: "Contents/Resources/dev-only.json",
  ...extra,
});

/** The xcodebuild product, the nested hub and the repo inputs of macSpec(). */
function macTree(f) {
  f.put("Products/Demo.app/Contents/Info.plist", "outer");
  f.put("Products/Demo.app/Contents/MacOS/Demo", "outer-exe");
  f.put("Products/Demo.app/Contents/PlugIns/DemoFinder.appex/Contents/Info.plist", "appex");
  f.put("Products/DemoHelper", "helper-exe");
  f.put("hub/bundle/DemoHub.app/Contents/Info.plist", "hub");
  f.put("mac/helper.plist", "<plist/>");
  f.put("mac/engine.plist", "<plist/>");
  f.put("third_party/lic/README", "license");
  f.receipt([{ name: "democli", content: "cli-bytes" }, { name: "smartctl", kind: "remote", content: "smart-bytes" }]);
}

const REQ = 'identity "Developer ID Application: Orthic Labs (6KLGD3LLKF)"';
function fakeRun(log, over = {}) {
  return (command, args) => {
    log.push([command, ...args].join(" "));
    if (command === "/usr/bin/plutil") {
      if (args.includes("CFBundleIdentifier")) return { status: 0, stdout: over.bundleId ?? "com.example.demo\n", stderr: "" };
      if (args.includes("BundleProgram")) return over.bundleProgram === null ? { status: 1, stdout: "", stderr: "" } : { status: 0, stdout: `${over.bundleProgram ?? "Contents/Helpers/DemoHelper"}\n`, stderr: "" };
    }
    if (command === "git") return { status: 0, stdout: args[0] === "rev-parse" ? "false\n" : "42\n", stderr: "" };
    if (command === "/usr/bin/codesign") {
      if (args.includes("-r-")) return { status: 0, stdout: `designated => ${over.requirement ?? REQ}\n`, stderr: "" };
      return { status: over.verifyStatus ?? 0, stdout: "", stderr: over.verifyStatus ? "invalid signature" : "" };
    }
    return { status: 0, stdout: "", stderr: "" };
  };
}

test("macOS assembly validation accepts the declared shape and fills defaults", () => {
  const spec = validateMacAssembly(macSpec());
  assert.equal(spec.stageReceipt, "dist/stage/stage-sidecar.json");
  assert.equal(spec.outer.configuration, "Release");
  assert.equal(spec.outer.product, "Demo.app");
  assert.equal(spec.stagingOutput, "dist/staging/Demo.app");
  assert.equal(spec.bindingFile, "dist/staging/payload-binding.json");
  assert.deepEqual(spec.helpers.map((helper) => helper.to), ["Contents/Helpers/DemoHelper", "Contents/Helpers/democli", "Contents/Helpers/smartctl", "Contents/Helpers/DemoHub.app/Contents/MacOS/ffmpeg"]);
  assert.equal(spec.launchDaemons[0].to, "Contents/Library/LaunchDaemons/helper.plist");
  assert.equal(spec.launchAgents[0].to, "Contents/Library/LaunchAgents/engine.plist");
});

test("macOS assembly validation refuses unknown keys, escapes, bad tools and ambiguous rows", () => {
  const bad = (extra, pattern) => assert.throws(() => validateMacAssembly(macSpec(extra)), (error) => error instanceof AssembledAppError && error.code === "ASSEMBLY_CONFIG_INVALID" && pattern.test(error.message));
  bad({ surprise: 1 }, /unknown key\(s\): surprise/);
  bad({ outer: { tool: "make", project: "a.xcodeproj", scheme: "S" } }, /outer\.tool/);
  bad({ outer: { tool: "xcodebuild", project: "/abs/a.xcodeproj", scheme: "S" } }, /relative/);
  bad({ helpers: [{ product: "A", stage: "B" }] }, /exactly one of/);
  bad({ helpers: [{ stage: "x", to: "../escape" }] }, /contained/);
  bad({ resources: [{ from: "a", to: "/abs" }] }, /relative/);
  bad({ bundleVersion: "date" }, /commit-count/);
  bad({ targetTriple: "x86_64-pc-windows-msvc" }, /apple-darwin/);
  bad({ nested: [{ built: "tauri", productName: "H", to: "Contents/Helpers/H.app" }] }, /exactly one of manifestPath, bundleDirectory/);
  bad({ outer: { tool: "xcodebuild", project: "a.xcodeproj", scheme: "S", extraBuildSettings: { CODE_SIGN_IDENTITY: "-" } } }, /CODE_SIGN/);
});

test("the macOS bundle is laid out from the xcodebuild product, nested app, helpers, plists and resources, hash-bound and version-stamped", (t) => {
  const f = fixture(t); macTree(f);
  const spec = validateMacAssembly(macSpec());
  const log = [];
  const out = path.join(f.root, "dist", "staging", "Demo.app");
  const receipt = loadStageReceipt({ root: f.root, spec, profile: "dev" });
  const { bound } = assembleMacBundle({ root: f.root, spec, profile: "dev", out, outerApp: path.join(f.root, "Products", "Demo.app"), nestedApps: [path.join(f.root, "hub", "bundle", "DemoHub.app")], receipt, deps: { run: fakeRun(log), log: () => {} } });
  for (const file of spec.require) assert.ok(fs.existsSync(path.join(out, ...file.split("/"))), file);
  assert.ok(fs.existsSync(path.join(out, "Contents/Library/LaunchAgents/engine.plist")));
  assert.equal(fs.statSync(path.join(out, "Contents/Helpers/democli")).mode & 0o111, 0o111, "helpers are executable");
  assert.equal(fs.existsSync(path.join(out, "Contents/Helpers/DemoHub.app/Contents/MacOS/ffmpeg")), false, "an optional row that is not staged is skipped");
  assert.equal(fs.existsSync(path.join(out, "Contents/Resources/ffmpeg")), false, "ifStaged resources follow their row");
  assert.deepEqual(bound.map((entry) => entry.dest).sort(), ["Contents/Helpers/democli", "Contents/Helpers/smartctl"]);
  assert.equal(bound.find((entry) => entry.dest === "Contents/Helpers/democli").sha256, sha("cli-bytes"));
  const stamped = log.filter((line) => line.includes("CFBundleVersion"));
  assert.equal(stamped.length, 3, "outer, nested app and plug-in are stamped");
  assert.ok(stamped.every((line) => line.includes("Set :CFBundleVersion 42")));
  const marker = JSON.parse(fs.readFileSync(path.join(out, "Contents/Resources/dev-only.json"), "utf8"));
  assert.deepEqual({ profile: marker.profile, devOnly: marker.devOnly }, { profile: "dev", devOnly: true });
  assert.equal(marker.files["Contents/Helpers/democli"], sha("cli-bytes"));
});

test("a release assembly writes no dev marker; a shallow clone is refused; a missing plug-in or required file fails", (t) => {
  const f = fixture(t); macTree(f);
  f.receipt([{ name: "democli", profile: "release", content: "cli-bytes" }, { name: "smartctl", kind: "remote", content: "smart-bytes" }]);
  const spec = validateMacAssembly(macSpec());
  const out = path.join(f.root, "out", "Demo.app");
  const base = { root: f.root, spec, profile: "release", out, outerApp: path.join(f.root, "Products", "Demo.app"), nestedApps: [path.join(f.root, "hub", "bundle", "DemoHub.app")], receipt: loadStageReceipt({ root: f.root, spec, profile: "release" }) };
  assembleMacBundle({ ...base, deps: { run: fakeRun([]), log: () => {} } });
  assert.equal(fs.existsSync(path.join(out, "Contents/Resources/dev-only.json")), false);
  const shallow = (command, args) => (command === "git" && args[0] === "rev-parse" ? { status: 0, stdout: "true\n", stderr: "" } : fakeRun([])(command, args));
  assert.throws(() => assembleMacBundle({ ...base, deps: { run: shallow, log: () => {} } }), /shallow/);
  fs.rmSync(path.join(f.root, "Products/Demo.app/Contents/PlugIns"), { recursive: true });
  assert.throws(() => assembleMacBundle({ ...base, deps: { run: fakeRun([]), log: () => {} } }), /DemoFinder\.appex/);
  macTree(f);
  f.receipt([{ name: "democli", profile: "release", content: "cli-bytes" }, { name: "smartctl", kind: "remote", content: "smart-bytes" }]);
  const wants = validateMacAssembly(macSpec({ require: ["Contents/Nope"] }));
  assert.throws(() => assembleMacBundle({ ...base, spec: wants, deps: { run: fakeRun([]), log: () => {} } }), /lacks Contents\/Nope/);
});

test("the stage receipt is re-hashed and its Cargo profile is enforced", (t) => {
  const f = fixture(t); macTree(f);
  const spec = validateMacAssembly(macSpec());
  assert.throws(() => loadStageReceipt({ root: f.root, spec, profile: "release" }).find("democli"), /staged with Cargo profile dev/);
  const receipt = loadStageReceipt({ root: f.root, spec, profile: "dev" });
  assert.equal(receipt.find("smartctl", "remote").sha256, sha("smart-bytes"), "remote rows carry no profile");
  assert.throws(() => receipt.find("nothing"), /no cargo-bin row nothing/);
  f.put("dist/stage/democli", "tampered");
  assert.throws(() => receipt.find("democli"), /differs from the stage receipt/);
  assert.throws(() => loadStageReceipt({ root: f.root, spec: { ...spec, targetTriple: "x86_64-apple-darwin" }, profile: "dev" }), /target/);
  fs.rmSync(path.join(f.root, "dist/stage/stage-sidecar.json"));
  assert.throws(() => loadStageReceipt({ root: f.root, spec, profile: "dev" }), (error) => error.code === "ASSEMBLY_RECEIPT_MISSING");
});

test("a Tauri bundle is found through Cargo's target directory by profile", (t) => {
  const f = fixture(t);
  const spec = validateMacAssembly(macSpec());
  const entry = validateMacAssembly(macSpec({ nested: [{ built: "tauri", manifestPath: "hub/src-tauri/Cargo.toml", productName: "DemoHub", to: "Contents/Helpers/DemoHub.app" }] })).nested[0];
  f.put("target/aarch64-apple-darwin/debug/bundle/macos/DemoHub.app/Contents/Info.plist");
  f.put("target/aarch64-apple-darwin/release/bundle/macos/DemoHub.app/Contents/Info.plist");
  const deps = { resolveTargetRoot: (manifest) => { assert.equal(manifest, path.join(f.root, "hub/src-tauri/Cargo.toml")); return path.join(f.root, "target"); } };
  assert.match(resolveNestedApp({ root: f.root, entry, spec, profile: "dev", deps }), /debug\/bundle\/macos\/DemoHub\.app$/);
  assert.match(resolveNestedApp({ root: f.root, entry, spec, profile: "release", deps }), /release\/bundle\/macos\/DemoHub\.app$/);
  fs.rmSync(path.join(f.root, "target/aarch64-apple-darwin/release"), { recursive: true });
  assert.throws(() => resolveNestedApp({ root: f.root, entry, spec, profile: "release", deps }), /not found/);
});

test("verifyMacBundle lists every structural problem, refuses a dev marker for release and runs /usr/bin/codesign when signed", (t) => {
  const f = fixture(t); macTree(f);
  const spec = validateMacAssembly(macSpec());
  const out = path.join(f.root, "Demo.app");
  assembleMacBundle({ root: f.root, spec, profile: "dev", out, outerApp: path.join(f.root, "Products", "Demo.app"), nestedApps: [path.join(f.root, "hub", "bundle", "DemoHub.app")], receipt: loadStageReceipt({ root: f.root, spec, profile: "dev" }), deps: { run: fakeRun([]), log: () => {} } });
  const log = [];
  assert.deepEqual(verifyMacBundle({ root: f.root, spec, bundle: out, profile: "dev", signed: true, deps: { run: fakeRun(log) } }), { checked: spec.require.length });
  assert.ok(log.includes(`/usr/bin/codesign --verify --strict --deep ${out}`));
  assert.throws(() => verifyMacBundle({ root: f.root, spec, bundle: out, profile: "release", deps: { run: fakeRun([]) } }), /dev-only payload/);
  assert.throws(() => verifyMacBundle({ root: f.root, spec, bundle: out, profile: "dev", deps: { run: fakeRun([], { bundleId: "com.other\n" }) } }), /CFBundleIdentifier "com.other"/);
  assert.throws(() => verifyMacBundle({ root: f.root, spec, bundle: out, profile: "dev", deps: { run: fakeRun([], { bundleProgram: "Contents/Helpers/Gone" }) } }), /BundleProgram Contents\/Helpers\/Gone is not in the bundle/);
  assert.throws(() => verifyMacBundle({ root: f.root, spec, bundle: out, profile: "dev", deps: { run: fakeRun([], { bundleProgram: "/abs/path" }) } }), /relative path inside the bundle/);
  assert.throws(() => verifyMacBundle({ root: f.root, spec, bundle: out, profile: "dev", signed: true, deps: { run: fakeRun([], { verifyStatus: 1 }) } }), /codesign --verify --strict --deep failed/);
  fs.rmSync(path.join(out, "Contents/Helpers/democli"));
  assert.throws(() => verifyMacBundle({ root: f.root, spec, bundle: out, profile: "dev", deps: { run: fakeRun([]) } }), /missing Contents\/Helpers\/democli/);
});

test("the release binding is written beside the output, re-hashes, and refuses a dev or changed payload", (t) => {
  const f = fixture(t);
  const spec = validateMacAssembly(macSpec());
  const out = path.join(f.root, "dist/staging/Demo.app");
  fs.mkdirSync(path.join(out, "Contents/Helpers"), { recursive: true });
  fs.writeFileSync(path.join(out, "Contents/Helpers/democli"), "cli-bytes");
  const file = writeReleaseBinding({ root: f.root, spec, bound: [{ dest: "Contents/Helpers/democli", sha256: sha("cli-bytes") }] });
  assert.equal(file, path.join(f.root, "dist/staging/payload-binding.json"));
  assert.equal(verifyReleaseBinding({ root: f.root, spec, output: out, rehash: true }).devOnly, false);
  fs.writeFileSync(path.join(out, "Contents/Helpers/democli"), "swapped");
  assert.throws(() => verifyReleaseBinding({ root: f.root, spec, output: out, rehash: true }), /changed after the receipt check/);
  assert.equal(verifyReleaseBinding({ root: f.root, spec, output: out, rehash: false }).profile, "release", "after signing the bytes change by design");
  fs.writeFileSync(file, JSON.stringify({ schema: 1, profile: "dev", devOnly: true, files: {} }));
  assert.throws(() => verifyReleaseBinding({ root: f.root, spec, output: out }), /dev-only|marks this payload/);
  fs.rmSync(file);
  assert.throws(() => verifyReleaseBinding({ root: f.root, spec, output: out }), (error) => error.code === "ASSEMBLY_BINDING_MISSING");
});

const winSpec = (extra = {}) => ({
  productName: "Demo", targetTriple: WIN_TRIPLE,
  payload: {
    files: [
      { stage: "demo-win", to: "Demo.exe" }, { stage: "democli", to: "Helpers/democli.exe" }, { stage: "launcher", to: "Helpers/launcher.exe" },
      { stage: "ort.dll", kind: "remote", to: "Helpers/runtime/ort.dll" },
      { from: "plugins/p", to: "plugins/p" },
      { stage: "launcher", to: "plugins/p/hooks/hook-start.exe" },
      { from: "NOTICE", to: "NOTICE.txt" },
    ],
    rewrites: [{ file: "plugins/p/hooks/hooks.json", regex: "\\$\\{ROOT\\}/hooks/(hook-[a-z]+\\.exe)", flags: "g", replacement: "%ROOT%\\\\hooks\\\\$1" }],
    require: ["Demo.exe", "NOTICE.txt", "plugins/p/hooks/hook-start.exe"],
  },
  sign: ["Demo.exe", "Helpers/democli.exe", "Helpers/launcher.exe"],
  devMarker: "dev-only.json",
  nsis: { script: "release/demo.nsi", output: "dist/releases/windows/Demo-Setup.exe", defines: { STAGE: "{payload}", OUT: "{output}", VERSION: "{version}" } },
  ...extra,
});
function winTree(f) {
  f.put("plugins/p/hooks/hooks.json", '{"command":"${ROOT}/hooks/hook-start.exe"}');
  f.put("plugins/p/hooks/hook-start.exe", "placeholder-script");
  f.put("NOTICE", "notice");
  f.receipt([{ name: "demo-win", content: "win-exe" }, { name: "democli", content: "cli-exe" }, { name: "launcher", content: "launcher-exe" }, { name: "ort.dll", kind: "remote", content: "ort-dll" }], WIN_TRIPLE);
}

test("the Windows payload is laid out, file rows overwrite copied folders, rewrites apply, and the dev marker is dev only", (t) => {
  const f = fixture(t); winTree(f);
  const spec = validateWindowsAssembly(winSpec());
  const out = path.join(f.root, "payload");
  const { bound } = assembleWindowsPayload({ root: f.root, spec, profile: "dev", out, receipt: loadStageReceipt({ root: f.root, spec, profile: "dev" }), deps: { log: () => {} } });
  assert.equal(fs.readFileSync(path.join(out, "plugins/p/hooks/hook-start.exe"), "utf8"), "launcher-exe", "the launcher replaces the script inside the copied plug-in folder");
  assert.equal(fs.readFileSync(path.join(out, "plugins/p/hooks/hooks.json"), "utf8"), '{"command":"%ROOT%\\\\hooks\\\\hook-start.exe"}');
  assert.equal(fs.readFileSync(path.join(out, "Helpers/runtime/ort.dll"), "utf8"), "ort-dll");
  assert.ok(bound.some((entry) => entry.dest === "plugins/p/hooks/hook-start.exe" && entry.sha256 === sha("launcher-exe")));
  assert.ok(fs.existsSync(path.join(out, "dev-only.json")));
  assert.deepEqual(verifyWindowsPayload({ spec, payload: out, profile: "dev" }), { checked: 6 });
  assert.throws(() => verifyWindowsPayload({ spec, payload: out, profile: "release" }), /dev-only/);
  const none = validateWindowsAssembly(winSpec({ payload: { ...winSpec().payload, rewrites: [{ file: "NOTICE.txt", regex: "zzz", replacement: "y" }] } }));
  assert.throws(() => assembleWindowsPayload({ root: f.root, spec: none, profile: "dev", out: path.join(f.root, "p2"), receipt: loadStageReceipt({ root: f.root, spec: none, profile: "dev" }), deps: { log: () => {} } }), /nothing matched/);
});

test("Windows validation refuses duplicate (case-insensitive) targets and a missing payload", () => {
  assert.throws(() => validateWindowsAssembly(winSpec({ payload: { files: [{ stage: "a", to: "A.exe" }, { stage: "b", to: "a.exe" }] } })), /twice/);
  assert.throws(() => validateWindowsAssembly(winSpec({ payload: { files: [] } })), /must not be empty/);
  assert.throws(() => validateWindowsAssembly(winSpec({ targetTriple: TRIPLE })), /windows triple/);
  assert.throws(() => resolveAssembly({}, "mac"), /declares no assembly\.mac/);
  assert.equal(validateWindowsAssembly(winSpec()).stagingOutput, "dist/staging/windows/Demo");
});

test("development validation accepts assembled-app and checks the assembly with the install keys", () => {
  const config = { assembly: { mac: macSpec() }, development: { targets: { mac: { build: [{ cmd: "node", args: ["x"] }], install: { kind: "assembled-app", destination: "/Applications/Demo.app", identifiers: { ".": "com.example.demo" }, verifyBinaries: ["Contents/MacOS/Demo"] } } } } };
  assert.doesNotThrow(() => validateDevelopmentConfig(config, "mac", { root: "/work" }));
  const wrongPath = structuredClone(config); wrongPath.development.targets.mac.install.destination = "/Applications/Other.app";
  assert.throws(() => validateDevelopmentConfig(wrongPath, "mac", { root: "/work" }), /production install path/);
  const noAssembly = structuredClone(config); delete noAssembly.assembly;
  assert.throws(() => validateDevelopmentConfig(noAssembly, "mac", { root: "/work" }), /no assembly\.mac/);
  const extra = structuredClone(config); extra.development.targets.mac.install.signing = "-";
  assert.throws(() => validateDevelopmentConfig(extra, "mac", { root: "/work" }), /unknown key\(s\): signing/);
  const winConfig = { assembly: { win: winSpec() }, development: { targets: { win: { build: [{ cmd: "node", args: ["x"] }], install: { kind: "assembled-app", installDirectory: "%LOCALAPPDATA%/Programs/Demo", stop: { scheduledTask: "Demo agent", process: "Demo.exe" }, restart: { scheduledTask: true, launch: "Demo.exe" } } } } } };
  assert.doesNotThrow(() => validateDevelopmentConfig(winConfig, "win", { root: "C:/work" }));
  winConfig.development.targets.win.install.restart.launch = "../x.exe";
  assert.throws(() => validateDevelopmentConfig(winConfig, "win", { root: "C:/work" }), /contained/);
});

test("assemble-app arguments", () => {
  assert.deepEqual(parseAssembleArgs(["--platform", "mac"]), { platform: "mac", profile: "release", build: false, verifyStaged: false, signed: false, package: false, config: undefined, out: undefined });
  assert.throws(() => parseAssembleArgs([]), /--platform/);
  assert.throws(() => parseAssembleArgs(["--platform", "mac", "--profile", "dev"]), /needs --out/);
  assert.throws(() => parseAssembleArgs(["--platform", "mac", "--signed"]), /--verify-staged/);
  assert.throws(() => parseAssembleArgs(["--platform", "mac", "--package"]), /win/);
  assert.throws(() => parseAssembleArgs(["--platform", "win", "--verify-staged", "--package"]), /separate/);
  assert.throws(() => parseAssembleArgs(["--platform", "mac", "--wat"]), /unknown/);
  assert.equal(parseAssembleArgs(["--platform", "win", "--profile", "dev", "--out", "o"]).out, "o");
});

test("production assemble-app (mac): prepare, outer build, bundle, binding, verify, then verify-staged after signing", async (t) => {
  const f = fixture(t); macTree(f);
  f.receipt([{ name: "democli", profile: "release", content: "cli-bytes" }, { name: "smartctl", kind: "remote", content: "smart-bytes" }]);
  const config = { assembly: { mac: macSpec({ prepare: { release: [{ cmd: "pnpm", args: ["exec", "right-release", "stage-sidecar"], env: { RIGHTKIT_BUILD_MODE: "release" } }] } }) } };
  const log = [];
  const run = (command, args, options) => { log.push(`${command} ${args.join(" ")}${options?.env?.RIGHTKIT_BUILD_MODE ? " [mode]" : ""}`); return fakeRun([])(command, args); };
  const deps = {
    run, log: () => {}, resolveCommand: (command, args) => ({ command, args }),
    storage: () => ({ derivedData: path.join(f.root, "DD") }),
    buildOuter: ({ storage, profile }) => { log.push(`build-outer ${profile}`); f.put("DD/Build/Products/Release/Demo.app/Contents/Info.plist", "outer"); f.put("DD/Build/Products/Release/Demo.app/Contents/MacOS/Demo", "exe"); f.put("DD/Build/Products/Release/Demo.app/Contents/PlugIns/DemoFinder.appex/Contents/Info.plist", "a"); f.put("DD/Build/Products/Release/DemoHelper", "h"); },
    commitCount: () => "7",
  };
  const result = await assembleApp({ root: f.root, config, opts: parseAssembleArgs(["--platform", "mac", "--build"]), env: {}, deps });
  assert.equal(result.output, path.join(f.root, "dist/staging/Demo.app"));
  assert.deepEqual(log.slice(0, 2), ["pnpm exec right-release stage-sidecar [mode]", "build-outer release"], "prepare before the outer build");
  const binding = JSON.parse(fs.readFileSync(path.join(f.root, "dist/staging/payload-binding.json"), "utf8"));
  assert.equal(binding.devOnly, false);
  assert.equal(binding.files["Contents/Helpers/democli"], sha("cli-bytes"));
  assert.equal(fs.existsSync(path.join(result.output, "Contents/Resources/dev-only.json")), false);
  const verified = await assembleApp({ root: f.root, config, opts: parseAssembleArgs(["--platform", "mac", "--verify-staged", "--signed"]), env: {}, deps });
  assert.equal(verified.verified, result.output);
  assert.ok(log.some((line) => line.startsWith("/usr/bin/codesign --verify --strict --deep")));
  fs.rmSync(path.join(f.root, "dist/staging/payload-binding.json"));
  await assert.rejects(assembleApp({ root: f.root, config, opts: parseAssembleArgs(["--platform", "mac", "--verify-staged"]), env: {}, deps }), /payload binding unreadable/);
});

test("production assemble-app (win): payload, binding, then makensis before the installer is signed", async (t) => {
  const f = fixture(t); winTree(f);
  f.receipt([{ name: "demo-win", profile: "release", content: "win-exe" }, { name: "democli", profile: "release", content: "cli-exe" }, { name: "launcher", profile: "release", content: "launcher-exe" }, { name: "ort.dll", kind: "remote", content: "ort-dll" }], WIN_TRIPLE);
  const config = { version: "1.2.3", assembly: { win: winSpec() } };
  const log = [];
  const run = (command, args) => { log.push([path.basename(command), ...args].join(" ")); if (/makensis/.test(command)) { const out = args.find((a) => a.startsWith("/DOUT=")).slice(6); fs.mkdirSync(path.dirname(out), { recursive: true }); fs.writeFileSync(out, "installer"); } return { status: 0, stdout: "", stderr: "" }; };
  const deps = { run, log: () => {}, resolveCommand: (command, args) => ({ command, args }), findMakensis: () => "C:/NSIS/makensis.exe" };
  const built = await assembleApp({ root: f.root, config, opts: parseAssembleArgs(["--platform", "win"]), env: {}, deps });
  assert.equal(built.output, path.join(f.root, "dist/staging/windows/Demo"));
  assert.ok(fs.existsSync(path.join(f.root, "dist/staging/windows/payload-binding.json")));
  await assembleApp({ root: f.root, config, opts: parseAssembleArgs(["--platform", "win", "--verify-staged"]), env: {}, deps });
  const packaged = await assembleApp({ root: f.root, config, opts: parseAssembleArgs(["--platform", "win", "--package"]), env: {}, deps });
  assert.equal(packaged.installer, path.join(f.root, "dist/releases/windows/Demo-Setup.exe"));
  assert.match(log.at(-2), /^makensis\.exe /, "makensis runs, then the signer");
  assert.match(log.at(-2), new RegExp(`/DSTAGE=${path.join(f.root, "dist/staging/windows/Demo").replaceAll("\\", "\\\\")} /DOUT=.*Demo-Setup\\.exe /DVERSION=1\\.2\\.3 .*demo\\.nsi$`));
  assert.match(log.at(-1), /sign-windows\.mjs .*Demo-Setup\.exe$/);
  assert.deepEqual(nsisArguments({ root: "/r", spec: validateWindowsAssembly(winSpec()), payload: "P", output: "O", version: "9" }), ["/V2", "/DSTAGE=P", "/DOUT=O", "/DVERSION=9", path.resolve("/r", "release/demo.nsi")]);
});

test("[GB-59] assemble --build: each prepare step takes the one build door (carried Cargo/pnpm launch with its declared env), an unsupported custom step is refused before anything is spawned, the dirty checkout is ignored", async (t) => {
  const { buildLaunch, BUILD_LAUNCH_REFUSED } = await import("./build-launch.mjs");
  const CLIENT = "/managed/agent-bin/rustright";
  const managed = { env: { PATH: "/managed/agent-bin" }, platform: "darwin", exists: (p) => p === CLIENT, which: (c) => (c === "rustright" ? [CLIENT] : []) };
  const door = (command, args, host) => buildLaunch(command, args, { ...managed, cwd: host.cwd, stepEnv: host.stepEnv });
  const run = (spawned) => (command, args, options) => { spawned.push({ command, args, env: options?.env }); return { status: 0 }; };
  const stepsOf = (release) => ({ assembly: { mac: macSpec({ prepare: { release } }) } });
  const common = (spawned) => ({ run: run(spawned), log: () => {}, resolveCommand: (command, args) => ({ command, args }), door, storage: () => ({ derivedData: "x" }), buildOuter: () => { throw new Error("stop after prepare"); } });
  // Cargo and pnpm: each is one broker-client launch (cargo by the client, pnpm as the rightkit-pnpm adapter job); never the raw tool.
  const f = fixture(t); macTree(f);
  f.receipt([{ name: "democli", profile: "release", content: "cli-bytes" }, { name: "smartctl", kind: "remote", content: "smart-bytes" }]);
  const spawned = [];
  await assert.rejects(assembleApp({ root: f.root, config: stepsOf([{ cmd: "cargo", args: ["build", "--release", "-p", "democli"] }, { cmd: "pnpm", args: ["run", "build"], env: { APP_BUILD_PROFILE: "release" } }]), opts: parseAssembleArgs(["--platform", "mac", "--build"]), env: {}, deps: common(spawned) }), /stop after prepare/);
  assert.deepEqual(spawned.map((s) => [s.command, s.args.slice(0, 3)]), [[CLIENT, ["cargo", "build", "--release"]], [CLIENT, ["run", "rightkit-pnpm", "--"]]], "every admitted step is one door launch");
  assert.ok(JSON.stringify(spawned[1]).includes("APP_BUILD_PROFILE"), "the step's declared env rides the adapter launch");
  // An unsupported custom command refuses before any spawn, naming the step.
  const refused = [];
  await assert.rejects(assembleApp({ root: f.root, config: stepsOf([{ cmd: "make", args: ["all"] }]), opts: parseAssembleArgs(["--platform", "mac", "--build"]), env: {}, deps: common(refused) }), (e) => /prepare command refused before it started/.test(e.message) && /no broker adapter runs this command/.test(e.message));
  assert.deepEqual(refused, [], "nothing was spawned");
  assert.ok(BUILD_LAUNCH_REFUSED);
});
