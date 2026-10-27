import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";

import { assertRealWindowsRoot, expandInstallDirectory, installAssembledMac, installAssembledWindows, outerBuildCommands, resolveAssembledMacDescriptor } from "./install-assembled-app.mjs";
import { validateMacAssembly, validateWindowsAssembly } from "./assembled-app.mjs";

const sha = (text) => createHash("sha256").update(text).digest("hex");
const REQ = 'identity "Developer ID Application: Orthic Labs (6KLGD3LLKF)"';
const DEST = "/Applications/Demo.app";

function workspace(t) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "rightkit-install-assembled-"));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const put = (relative, content = "x") => {
    const file = path.join(root, ...relative.split("/"));
    fs.mkdirSync(path.dirname(file), { recursive: true });
    fs.writeFileSync(file, content);
    return file;
  };
  const receipt = (target, rows) => {
    const files = rows.map(({ name, content }) => { put(`dist/stage/${name}`, content); return { kind: "cargo-bin", name, profile: "dev", path: `dist/stage/${name}`, sha256: sha(content), sizeBytes: content.length }; });
    put("dist/stage/stage-sidecar.json", JSON.stringify({ schema: 1, contract: "rightkit-stage-sidecar-v1", target, files }));
  };
  return { root, put, receipt };
}

/** A node:fs that sends a prefix (the real /Applications/Demo.app) into the temp workspace. */
function redirect(map) {
  const re = (value) => { if (typeof value === "string") for (const [from, to] of map) if (value.startsWith(from)) return to + value.slice(from.length); return value; };
  return new Proxy(fs, { get: (target, prop) => (typeof target[prop] === "function" ? (...args) => target[prop](...args.map(re)) : target[prop]) });
}

const macSpec = () => validateMacAssembly({
  productName: "Demo", bundleId: "com.example.demo", targetTriple: "aarch64-apple-darwin",
  outer: { tool: "xcodebuild", projectSpec: "mac/project.yml", project: "mac/Demo.xcodeproj", scheme: "Demo", unsetEnv: ["DEMO_QA"] },
  helpers: [{ stage: "democli" }],
  launchDaemons: [{ from: "mac/helper.plist" }],
  require: ["Contents/MacOS/Demo", "Contents/Helpers/democli"],
});
const macConfig = { development: { targets: { mac: { install: { kind: "assembled-app", destination: DEST, identifiers: { ".": "com.example.demo" }, verifyBinaries: ["Contents/MacOS/Demo", "Contents/Helpers/democli"] } } } } };

function macHarness(t, { existing = false, failStaged = false, mismatchInstalled = false, requirement = REQ, adhoc = false } = {}) {
  const w = workspace(t);
  w.put("mac/helper.plist", "<plist/>");
  w.receipt("aarch64-apple-darwin", [{ name: "democli", content: "cli" }]);
  const events = [];
  const realDest = path.join(w.root, "Applications", "Demo.app");
  if (existing) { fs.mkdirSync(path.join(realDest, "Contents"), { recursive: true }); fs.writeFileSync(path.join(realDest, "Contents", "old"), "old"); }
  const deps = {
    fs: redirect([[DEST, realDest]]),
    storage: () => ({ derivedData: path.join(w.root, "DD") }),
    buildOuter: ({ env }) => {
      events.push(`build-outer unset=${"DEMO_QA" in env}`);
      w.put("DD/Build/Products/Release/Demo.app/Contents/Info.plist", "outer");
      w.put("DD/Build/Products/Release/Demo.app/Contents/MacOS/Demo", "exe");
    },
    commitCount: () => "9",
    makeTemp: () => fs.mkdtempSync(path.join(w.root, "stage-")),
    sign: async ({ bundle, descriptor }) => { events.push(`sign ${path.basename(bundle)} id=${descriptor.bundleId}`); return { identity: "Developer ID Application: Orthic Labs (6KLGD3LLKF)", teamIdentifier: "6KLGD3LLKF", designatedRequirement: adhoc ? null : requirement, adhoc }; },
    assertRequirement: async ({ installed, expected }) => { events.push("assert-requirement"); if (expected !== REQ) { const e = new Error("changed"); e.code = "DEV_MAC_SIGNING_REQUIREMENT_CHANGED"; throw e; } },
    assertIdentities: () => events.push("assert-identities-installed"),
    installApp: async ({ app, dest, appName, dev }) => {
      events.push(`install dev=${dev} ${dest}/${appName}`);
      fs.rmSync(realDest, { recursive: true, force: true });
      fs.cpSync(app, realDest, { recursive: true });
      if (mismatchInstalled) fs.writeFileSync(path.join(realDest, "Contents/Helpers/democli"), "tampered");
    },
    run: (command, args) => {
      if (command === "/usr/bin/plutil") return args.includes("CFBundleIdentifier") ? { status: 0, stdout: "com.example.demo\n", stderr: "" } : { status: 1, stdout: "", stderr: "no such key" };
      if (command === "/usr/bin/codesign") {
        const target = args.at(-1);
        events.push(`codesign ${args.includes("-r-") ? "requirement" : "verify"} ${target.startsWith(DEST) || target.startsWith(realDest) ? "installed" : "staged"}`);
        if (args.includes("-r-")) return { status: 0, stdout: `designated => ${requirement}\n`, stderr: "" };
        return failStaged && !target.startsWith(DEST) ? { status: 1, stdout: "", stderr: "a nested signature is invalid" } : { status: 0, stdout: "", stderr: "" };
      }
      return { status: 0, stdout: "", stderr: "" };
    },
    log: () => {},
  };
  return { w, events, deps, realDest, run: (extra = {}) => installAssembledMac({ root: w.root, config: macConfig, descriptor: macConfig.development.targets.mac.install, spec: macSpec(), env: { DEMO_QA: "1" }, deps: { ...deps, ...extra } }) };
}

test("mac: build, sign, verify the STAGED bundle, assert the requirement, install in place, verify the installed copy, in that order", async (t) => {
  const h = macHarness(t, { existing: true });
  const result = await h.run();
  assert.deepEqual(h.events, [
    "build-outer unset=true", "sign Demo.app id=com.example.demo",
    "codesign verify staged", "codesign requirement staged", "assert-requirement",
    "install dev=true /Applications/Demo.app", "codesign verify installed", "codesign requirement installed",
  ], "identity assertions run on the staged copy inside signMacosDevelopmentBundle, and on the installed copy when declared");
  assert.equal(result.installed, DEST);
  assert.equal(result.designatedRequirement, REQ);
  assert.equal(fs.existsSync(path.join(h.realDest, "Contents", "old")), false, "the old app is replaced in place");
  assert.equal(fs.existsSync(`${h.realDest}.previous`), false, "no backup of the old app");
  assert.equal(fs.readFileSync(path.join(h.realDest, "Contents/Helpers/democli"), "utf8"), "cli");
  assert.equal(fs.readdirSync(h.w.root).some((name) => name.startsWith("stage-")), false, "the private staging folder is removed");
});

test("mac: a staged bundle that fails verification never touches the installed app", async (t) => {
  const h = macHarness(t, { existing: true, failStaged: true });
  await assert.rejects(h.run(), /staged .* failed codesign --verify --strict --deep/);
  assert.ok(!h.events.some((line) => line.startsWith("install")), "installApp is never called");
  assert.ok(!h.events.includes("assert-requirement"));
  assert.equal(fs.readFileSync(path.join(h.realDest, "Contents", "old"), "utf8"), "old", "the destination is untouched");
});

test("mac: a changed designated requirement stops before the install; ad hoc signing is refused", async (t) => {
  const changed = macHarness(t, { existing: true, requirement: 'identity "Developer ID Application: Someone Else (AAAAAAAAAA)"' });
  await assert.rejects(changed.run(), /changed/);
  assert.ok(!changed.events.some((line) => line.startsWith("install")));
  assert.equal(fs.existsSync(path.join(changed.realDest, "Contents", "old")), true);
  const adhoc = macHarness(t, { adhoc: true });
  await assert.rejects(adhoc.run(), (error) => error.code === "ASSEMBLY_SIGNING_UNSTABLE");
  assert.ok(!adhoc.events.some((line) => line.startsWith("install")));
});

test("mac: an installed copy that differs from the signed staged copy is removed (no old copy kept)", async (t) => {
  const h = macHarness(t, { existing: true, mismatchInstalled: true });
  await assert.rejects(h.run(), (error) => /installed Contents\/Helpers\/democli does not match the signed staged copy/.test(error.message) && error.rolledBack === "removed-failed-install");
  assert.equal(fs.existsSync(h.realDest), false);
  assert.equal(fs.existsSync(`${h.realDest}.previous`), false);
});

test("mac descriptor rules: the production path, the assembly bundle id, no other keys", () => {
  const spec = macSpec();
  const ok = resolveAssembledMacDescriptor(macConfig.development.targets.mac.install, spec, { config: macConfig });
  assert.equal(ok.bundleId, "com.example.demo");
  assert.throws(() => resolveAssembledMacDescriptor({ ...macConfig.development.targets.mac.install, destination: "/Applications/Demo.dev.app" }, spec), /production install path/);
  assert.throws(() => resolveAssembledMacDescriptor({ ...macConfig.development.targets.mac.install, bundleId: "com.example.other" }, spec), /differs from assembly\.mac\.bundleId/);
  assert.throws(() => resolveAssembledMacDescriptor({ ...macConfig.development.targets.mac.install, verifyBinaries: ["../x"] }, spec), /contained/);
  assert.throws(() => resolveAssembledMacDescriptor({ kind: "tauri-devid", destination: DEST }, spec), /unknown key|kind/);
});

test("the outer build commands: xcodegen, then an unsigned xcodebuild in the managed DerivedData", () => {
  const commands = outerBuildCommands({ root: "/app", spec: macSpec(), storage: { derivedData: "/managed/DD", resultBundle: "/managed/r.xcresult" }, profile: "dev" });
  assert.deepEqual(commands.map((c) => c.cmd), ["xcodegen", "xcodebuild"]);
  assert.deepEqual(commands[0].args, ["generate", "--spec", path.resolve("/app", "mac/project.yml"), "--project", path.resolve("/app", "mac")]);
  const args = commands[1].args;
  assert.equal(args[args.indexOf("-derivedDataPath") + 1], "/managed/DD");
  assert.ok(args.includes("CODE_SIGNING_ALLOWED=NO") && args.at(-1) === "build");
  assert.equal(args[args.indexOf("-resultBundlePath") + 1], "/managed/r.xcresult");
});

// ---- Windows ------------------------------------------------------------------------------------------------------

const winSpec = () => validateWindowsAssembly({
  productName: "Demo", targetTriple: "x86_64-pc-windows-msvc",
  payload: { files: [{ stage: "demo-win", to: "Demo.exe" }, { stage: "launcher", to: "Helpers/launcher.exe" }], require: ["Demo.exe"] },
  sign: ["Demo.exe", "Helpers/launcher.exe"],
});
const winInstall = { kind: "assembled-app", installDirectory: "%LOCALAPPDATA%/Programs/Demo", stop: { scheduledTask: "Demo agent", process: "Demo.exe" }, restart: { scheduledTask: true, launch: "Demo.exe" }, carryOver: ["Uninstall.exe"] };

function winHarness(t, over = {}) {
  const w = workspace(t);
  w.receipt("x86_64-pc-windows-msvc", [{ name: "demo-win", content: "NEW-exe" }, { name: "launcher", content: "launcher" }]);
  const local = path.join(w.root, "local");
  const dir = path.join(local, "Programs", "Demo");
  fs.mkdirSync(dir, { recursive: true });
  fs.writeFileSync(path.join(dir, "Demo.exe"), "OLD-exe");
  fs.writeFileSync(path.join(dir, "Uninstall.exe"), "uninstaller");
  const events = [];
  const state = (label) => `${label}:${fs.existsSync(path.join(dir, "Demo.exe")) ? fs.readFileSync(path.join(dir, "Demo.exe"), "utf8") : "absent"}`;
  let stopCalls = 0;
  const base = fs;
  const deps = {
    fs: over.fs ?? base, pid: process.pid, isAlive: () => false, sleep: () => {}, log: () => {},
    sign: (files) => { events.push(`sign ${files.length}`); return { status: over.signStatus ?? 0 }; },
    verifySigned: () => { events.push("verify-signed"); return { status: over.verifyStatus ?? 0 }; },
    isRunning: () => true,
    endTask: (name) => events.push(`end-task ${name}`),
    stopInDirectory: () => { stopCalls += 1; events.push(state("stop")); },
    anyInDirectory: () => (over.alwaysRunning ? true : stopCalls < 2),
    runTask: (name) => events.push(`run-task ${name}`),
    launch: () => events.push(state("launch")),
    ...(over.now ? { now: over.now } : {}),
  };
  const run = () => installAssembledWindows({ root: w.root, config: {}, descriptor: winInstall, spec: winSpec(), env: { LOCALAPPDATA: local }, deps: { ...deps, assertProfile: fixtureProfile } });
  return { w, dir, events, run, deps };
}

test("windows: assemble, sign and prove the incoming payload, stop, swap by rename, restart from the new copy, delete the old folder", (t) => {
  const h = winHarness(t);
  const result = h.run();
  assert.deepEqual(h.events, ["sign 2", "verify-signed", "end-task Demo agent", "stop:OLD-exe", "stop:OLD-exe", "run-task Demo agent", "launch:NEW-exe"]);
  assert.equal(fs.readFileSync(path.join(h.dir, "Demo.exe"), "utf8"), "NEW-exe");
  assert.equal(fs.readFileSync(path.join(h.dir, "Uninstall.exe"), "utf8"), "uninstaller", "installer-owned files are carried over");
  for (const leftover of [".incoming", ".replaced", ".install-lock", ".previous"]) assert.equal(fs.existsSync(`${h.dir}${leftover}`), false, leftover);
  assert.deepEqual({ files: result.files, signed: result.signed, launched: result.launched, warnings: result.warnings }, { files: 2, signed: 2, launched: true, warnings: [] });
});

test("windows: a failed signature leaves the install untouched, stops nothing and removes the incoming copy", (t) => {
  for (const over of [{ signStatus: 1 }, { verifyStatus: 1 }]) {
    const h = winHarness(t, over);
    assert.throws(() => h.run(), /the install was not touched/);
    assert.equal(fs.readFileSync(path.join(h.dir, "Demo.exe"), "utf8"), "OLD-exe");
    assert.ok(!h.events.some((line) => line.startsWith("end-task") || line.startsWith("stop")), "nothing was stopped");
    assert.equal(fs.existsSync(`${h.dir}.incoming`), false);
    assert.equal(fs.existsSync(`${h.dir}.install-lock`), false);
  }
});

test("windows: processes that will not stop restore the old copy and leave the install untouched", (t) => {
  let tick = 0;
  const h = winHarness(t, { alwaysRunning: true, now: () => (tick += 20000) });
  assert.throws(() => h.run(), /still run from the install folder/);
  assert.equal(fs.readFileSync(path.join(h.dir, "Demo.exe"), "utf8"), "OLD-exe");
  assert.ok(h.events.includes("run-task Demo agent"), "the background task is started again");
  assert.equal(fs.existsSync(`${h.dir}.incoming`), false);
});

test("windows: a busy folder is retried after stopping what runs from it", (t) => {
  let busy = 1;
  const wrapped = new Proxy(fs, { get: (target, prop) => (prop === "renameSync" ? (from, to) => { if (busy-- > 0 && String(to).endsWith(".replaced")) { const e = new Error("busy"); e.code = "EBUSY"; throw e; } return target.renameSync(from, to); } : typeof target[prop] === "function" ? target[prop].bind(target) : target[prop]) });
  const h = winHarness(t, { fs: wrapped });
  h.run();
  assert.equal(fs.readFileSync(path.join(h.dir, "Demo.exe"), "utf8"), "NEW-exe");
  assert.ok(h.events.filter((line) => line.startsWith("stop")).length >= 3, "stopped again before the retry");
});

test("windows: a live lock refuses, a stale lock is taken over, and an install that stopped between its renames is put back first", (t) => {
  const live = winHarness(t);
  fs.writeFileSync(`${live.dir}.install-lock`, "4242");
  live.deps.isAlive = () => true;
  assert.throws(() => live.run(), /another install \(pid 4242\) is running/);
  assert.equal(fs.readFileSync(`${live.dir}.install-lock`, "utf8"), "4242", "the other run's lock is not removed");
  const stale = winHarness(t);
  fs.writeFileSync(`${stale.dir}.install-lock`, "4242");
  stale.run();
  assert.equal(fs.readFileSync(path.join(stale.dir, "Demo.exe"), "utf8"), "NEW-exe");
  const half = winHarness(t);
  fs.renameSync(half.dir, `${half.dir}.replaced`);
  half.run();
  assert.equal(fs.readFileSync(path.join(half.dir, "Uninstall.exe"), "utf8"), "uninstaller", "the missing install folder got its old copy back, then was replaced");
});

// Fixture LOCALAPPDATA folders are temp folders, not C:\\Users\\<name>\\AppData\\Local: the profile-shape check (tested on its own below)
// is replaced for the tests of the other behaviour, which otherwise fail on a real Windows host (Dell native run, 2026-10-27).
const fixtureProfile = () => {};

test("windows: install directory expansion and descriptor checks", () => {
  assert.equal(expandInstallDirectory("%LOCALAPPDATA%/Programs/Demo", { LOCALAPPDATA: path.resolve("/u/local") }, { assertProfile: fixtureProfile }), path.normalize(path.resolve("/u/local", "Programs/Demo")));
  assert.throws(() => expandInstallDirectory("%APPDATA%/Demo", { APPDATA: "/a" }), /may only expand/);
  assert.throws(() => expandInstallDirectory("%LOCALAPPDATA%/Demo", {}), /is not set/);
  assert.throws(() => expandInstallDirectory("relative/dir", {}), /absolute/);
  assert.throws(() => expandInstallDirectory(path.parse(process.cwd()).root, {}), /non-root/);
});

test("windows: a redirected or non-standard LOCALAPPDATA is refused, the real one and an absolute installDirectory pass", () => {
  const real = "C:\\Users\\adrds\\AppData\\Local";
  assert.doesNotThrow(() => assertRealWindowsRoot("%LOCALAPPDATA%/Programs/Pulse", `${real}\\Programs\\Pulse`, { LOCALAPPDATA: real }));
  for (const bad of ["C:\\Users\\adrds\\ClaudeProfiles\\x\\AppData\\Local", "D:\\scratch\\profile\\Local", "C:\\Users\\adrds\\AppData\\Local\\Temp"]) {
    assert.throws(() => expandInstallDirectory("%LOCALAPPDATA%/Programs/Pulse", { LOCALAPPDATA: bad }), (e) => e.message.includes(bad) && /redirected or non-standard/.test(e.message), bad);
  }
  assert.throws(() => assertRealWindowsRoot("C:\\Users\\a\\ClaudeProfiles\\p\\Programs\\Pulse", "C:\\Users\\a\\ClaudeProfiles\\p\\Programs\\Pulse", {}), /ClaudeProfiles/);
});

test("windows: a LOCALAPPDATA root without the running app is refused when another user's Programs<Product> holds it", (t) => {
  const h = winHarness(t);
  const peer = "C:\\Users\\adrds\\AppData\\Local\\Programs\\Demo";
  const attempt = (extra) => installAssembledWindows({ root: h.w.root, config: {}, descriptor: { ...winInstall, installDirectory: "%LOCALAPPDATA%/Programs/Demo" }, spec: winSpec(), env: { LOCALAPPDATA: path.dirname(path.dirname(h.dir)) }, deps: { ...h.deps, assertProfile: fixtureProfile, ...extra } });
  assert.throws(() => attempt({ peerInstalls: () => [peer], runningDirectories: () => [peer] }), /does not hold the running Demo/);
  assert.ok(!h.events.some((line) => line.startsWith("end-task") || line.startsWith("stop")), "nothing was stopped");
  assert.doesNotThrow(() => attempt({ peerInstalls: () => [peer], runningDirectories: () => [h.dir] }));
  // Unknown is never "no conflict": a discovery that cannot see refuses before anything is stopped.
  const blind = () => { throw new Error("EACCES"); };
  const before = h.events.length;
  assert.throws(() => attempt({ peerInstalls: blind, runningDirectories: () => [] }), (e) => e.code === "ASSEMBLY_INSTALL_FAILED" && /peer install discovery failed/.test(e.message));
  assert.throws(() => attempt({ peerInstalls: () => [peer], runningDirectories: blind }), (e) => e.code === "ASSEMBLY_INSTALL_FAILED" && /running process discovery failed/.test(e.message));
  assert.ok(!h.events.slice(before).some((line) => line.startsWith("end-task") || line.startsWith("stop")), "nothing was stopped after a failed discovery");
});
