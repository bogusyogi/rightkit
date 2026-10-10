import test from "node:test";
import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileEvidence } from "./macos-file-evidence.mjs";
import { expandMacSigningEntries } from "./mac-bundle-signing.mjs";
import { buildStapledSwiftMacDmg, swiftMacDmgConfigError, swiftMacDmgPlan } from "./swift-mac-dmg.mjs";

const target = (extra = {}) => ({ prePackage: { cmd: "swift", args: ["build"] }, notarize: { app: "dist/App.app", file: "dist/App.dmg", appBeforePackage: true }, ...extra });

test("DMG is built after the app is stapled, then signed, in that order", async () => {
  const calls = [];
  const plan = swiftMacDmgPlan({ target: target({ dmg: { volname: "Slice" } }), projectType: "swiftpm", resolve: (p) => `/r/${p}` });
  assert.equal(plan.app, "/r/dist/App.app");
  assert.equal(plan.out, "/r/dist/App.dmg");
  await buildStapledSwiftMacDmg(plan, {
    run: (cmd, args) => { calls.push(`${cmd} ${args.join(" ")}`); return { status: 0 }; },
    build: (o) => { calls.push(`build ${o.app} ${o.out} ${o.volname}`); return { out: o.out }; },
    signDmg: async (f) => { calls.push(`sign ${f}`); },
    remove: (f) => calls.push(`rm ${f}`),
    log: () => {},
  });
  assert.deepEqual(calls, ["xcrun stapler validate /r/dist/App.app", "rm /r/dist/App.dmg", "build /r/dist/App.app /r/dist/App.dmg Slice", "sign /r/dist/App.dmg"]);
});

test("an unstapled app is never packed", async () => {
  const plan = swiftMacDmgPlan({ target: target(), projectType: "swiftpm" });
  let built = false;
  await assert.rejects(buildStapledSwiftMacDmg(plan, { run: () => ({ status: 65 }), build: () => { built = true; }, log: () => {} }), /not stapled/);
  assert.equal(built, false);
});

test("plan applies only to swiftpm + appBeforePackage without a package step or dmg:false", () => {
  assert.equal(swiftMacDmgPlan({ target: target(), projectType: "tauri" }), null);
  assert.equal(swiftMacDmgPlan({ target: target({ dmg: false }), projectType: "swiftpm" }), null);
  assert.equal(swiftMacDmgPlan({ target: target({ package: { cmd: "x" } }), projectType: "swiftpm" }), null);
  assert.equal(swiftMacDmgPlan({ target: target({ notarize: { app: "a.app", file: "a.dmg" } }), projectType: "swiftpm" }), null);
});

test("preflight rejects a prePackage that builds the DMG before the app is stapled", () => {
  const bad = swiftMacDmgConfigError({ target: target({ prePackage: { cmd: "node", args: ["scripts/make-dmg.mjs"] } }), projectType: "swiftpm", appName: "SliceApp" });
  assert.match(bad, /prePackage builds a DMG.*unstapled/);
  assert.equal(swiftMacDmgConfigError({ target: target(), projectType: "swiftpm" }), null);
  assert.equal(swiftMacDmgConfigError({ target: target({ prePackage: { cmd: "make-dmg" } }), projectType: "tauri" }), null);
  assert.match(swiftMacDmgConfigError({ target: target({ notarize: { app: "a.app", file: "a.zip", appBeforePackage: true } }), projectType: "swiftpm" }), /must be a \.dmg/);
});

test("signing entries: .app directory yields nested code, others are rejected, files pass through", () => {
  const seen = [];
  const out = expandMacSigningEntries(["/x/bin", "/x/App.app"], {
    isDirectory: (f) => f.endsWith(".app"), exists: () => true, mainName: () => "App",
    discover: (app, main) => { seen.push([app, main]); return ["/x/App.app/Contents/Frameworks/F.framework", "/x/App.app/Contents/Frameworks/F.framework/F"]; },
  });
  assert.deepEqual(seen, [["/x/App.app", "App"]]);
  assert.equal(out.nested.length, 2);
  assert.deepEqual(out.files, ["/x/bin", "/x/App.app"]);
  assert.throws(() => expandMacSigningEntries(["/x/dir"], { isDirectory: () => true }), /not a \.app bundle/);
  assert.deepEqual(expandMacSigningEntries(["/x/bin"], { isDirectory: () => false }).nested, []);
});

test("fileEvidence hashes a bundle directory instead of crashing with EISDIR", () => {
  const app = path.join(mkdtempSync(path.join(os.tmpdir(), "rk-bundle-")), "App.app");
  mkdirSync(path.join(app, "Contents", "MacOS"), { recursive: true });
  writeFileSync(path.join(app, "Contents", "MacOS", "App"), "x");
  const a = fileEvidence(app);
  assert.equal(a.kind, "bundle-tree-v1");
  writeFileSync(path.join(app, "Contents", "MacOS", "App"), "y");
  assert.notEqual(fileEvidence(app).sha256, a.sha256);
});

test("an appdmg spec supplies the volume name; targets.mac.dmg options and the app name are fallbacks", async () => {
  const built = [];
  const run = () => ({ status: 0 });
  const build = (o) => { built.push(o); return { out: o.out }; };
  const dir = mkdtempSync(path.join(os.tmpdir(), "rk-swift-dmg-"));
  try {
    const spec = path.join(dir, "appdmg.json");
    writeFileSync(spec, JSON.stringify({ title: "Spec Title", window: { size: { width: 600, height: 400 } } }));
    const plan = swiftMacDmgPlan({ target: target({ dmg: { spec: "appdmg.json" } }), projectType: "swiftpm", appName: "App", resolve: (p) => path.join(dir, p) });
    assert.equal(plan.options.spec, spec);
    assert.equal(plan.options.volname, undefined);
    await buildStapledSwiftMacDmg(plan, { run, build, log: () => {} });
    assert.equal(built[0].volname, "Spec Title"); assert.equal(built[0].spec, spec);
    // No title in the spec: the app name is the volume name. An explicit dmg.volname wins over the spec title.
    writeFileSync(spec, JSON.stringify({ window: { size: { width: 600, height: 400 } } }));
    await buildStapledSwiftMacDmg(plan, { run, build, log: () => {} });
    assert.equal(built[1].volname, "App");
    const explicit = swiftMacDmgPlan({ target: target({ dmg: { spec: "appdmg.json", volname: "Mine" } }), projectType: "swiftpm", appName: "App", resolve: (p) => path.join(dir, p) });
    assert.equal(explicit.options.volname, "Mine");
  } finally { rmSync(dir, { recursive: true, force: true }); }
});
