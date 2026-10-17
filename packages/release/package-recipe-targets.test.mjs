import assert from "node:assert/strict";
import path from "node:path";
import test from "node:test";
import { expandMacPackageTarget, expandPackageRecipeTarget, expandWinPackageTarget, PackageRecipeTargetError } from "./package-recipe-targets.mjs";
import { parseStageSidecarArgs } from "./stage-sidecar-command.mjs";
import { packageCommandOptional } from "./project-type.mjs";
import { swiftMacDmgPlan } from "./swift-mac-dmg.mjs";

const appRoot = path.resolve("fixture-app");
const NSIS = "windows-raw-exe-authenticode-before-nsis-v1";
const winTarget = (extra = {}) => ({
  signed: true,
  winPackage: { installer: "nsis", productName: "Demo", updater: true },
  sign: { prePackageFiles: ["src-tauri/target/x86_64-pc-windows-msvc/release/demo.exe"], files: ["src-tauri/target/x86_64-pc-windows-msvc/release/bundle/nsis/Demo_1.0.0_x64-setup.exe"] },
  ...extra,
});

test("winPackage expands to the two-phase NSIS hooks release.mjs already gates", () => {
  const out = expandWinPackageTarget(winTarget(), { appRoot, config: { packageManager: "pnpm" } });
  assert.equal(out.winPackage, undefined);
  assert.equal(out.signingContract, NSIS);
  assert.deepEqual(out.prePackage.args.slice(0, 4), ["exec", "tauri", "build", "--no-bundle"]);
  assert.deepEqual(out.package.args.slice(0, 5), ["exec", "tauri", "bundle", "--bundles", "nsis"]);
  assert.equal(JSON.parse(out.package.args.at(-1)).bundle.createUpdaterArtifacts, false);
  assert.equal(out.prePackage.env.TAURI_SIGNING_PRIVATE_KEY, "");
  assert.deepEqual(out.sign.prePackageFiles, ["src-tauri/target/x86_64-pc-windows-msvc/release/demo.exe"]);
  assert.equal(out.sign.files.length, 1);
});

test("winPackage refuses a hook script, sidecars and another signing contract", () => {
  const cfg = { appRoot, config: {} };
  assert.throws(() => expandWinPackageTarget(winTarget({ prePackage: { cmd: "node" } }), cfg), PackageRecipeTargetError);
  assert.throws(() => expandWinPackageTarget(winTarget({ package: { cmd: "node" } }), cfg), PackageRecipeTargetError);
  assert.throws(() => expandWinPackageTarget(winTarget({ winPackage: { installer: "nsis", productName: "Demo", updater: true, sidecars: ["a.exe"] } }), cfg), /externalBin/);
  assert.throws(() => expandWinPackageTarget(winTarget({ signingContract: "windows-msix-signed-v1" }), cfg), /signingContract/);
  assert.throws(() => expandWinPackageTarget(winTarget({ winPackage: { installer: "nsis", productName: "Demo" } }), cfg), /updater/);
});

const macTarget = (extra = {}) => ({
  signed: true,
  macPackage: {},
  notarize: { app: "src-tauri/target/universal-apple-darwin/release/bundle/macos/Demo.app", file: "dist/Demo_universal.dmg", profile: "apple-dev-notary" },
  ...extra,
});

test("macPackage expands to a universal app build, app-first notarization and right-release's own DMG", () => {
  const out = expandMacPackageTarget(macTarget(), { appRoot });
  assert.equal(out.macPackage, undefined);
  assert.ok(out.prePackage.args.includes("universal-apple-darwin"));
  assert.ok(out.prePackage.args.includes("--no-sign"));
  assert.deepEqual(out.sign.prePackageFiles, [out.notarize.app]);
  assert.equal(out.notarize.appBeforePackage, true);
  assert.equal(out.package, undefined);
  const plan = swiftMacDmgPlan({ target: out, projectType: "tauri", appName: "Demo", resolve: (p) => p });
  assert.equal(plan.app, out.notarize.app);
  assert.equal(plan.out, "dist/Demo_universal.dmg");
  // A plain Tauri target with no recipe still builds its own DMG.
  assert.equal(swiftMacDmgPlan({ target: { notarize: { appBeforePackage: true, app: "a.app", file: "a.dmg" } }, projectType: "tauri" }), null);
});

test("macPackage refuses hooks, a non-DMG notarize file, dmg: false and unknown keys", () => {
  assert.throws(() => expandMacPackageTarget(macTarget({ prePackage: { cmd: "node" } }), { appRoot }), PackageRecipeTargetError);
  assert.throws(() => expandMacPackageTarget(macTarget({ notarize: { app: "a/Demo.app", file: "a.zip" } }), { appRoot }), /\.dmg/);
  assert.throws(() => expandMacPackageTarget(macTarget({ dmg: false }), { appRoot }), /dmg: false/);
  assert.throws(() => expandMacPackageTarget(macTarget({ macPackage: { lipo: true } }), { appRoot }), /unknown key/);
  assert.throws(() => expandMacPackageTarget(macTarget({ sign: { prePackageFiles: ["other.app"] } }), { appRoot }), /must include notarize.app/);
});

test("targets without a recipe pass through, and recipes are refused off their platform", () => {
  const plain = { signed: true, package: { cmd: "node" } };
  assert.equal(expandPackageRecipeTarget(plain, { platform: "win", appRoot, config: {} }), plain);
  assert.equal(expandPackageRecipeTarget(undefined, { platform: "mac", appRoot, config: {} }), undefined);
  assert.throws(() => expandPackageRecipeTarget({ macPackage: {} }, { platform: "android", appRoot, config: {} }), PackageRecipeTargetError);
  assert.equal(expandPackageRecipeTarget({ signed: true, recipe: { kind: "macPackage" } }, { platform: "mac", appRoot, config: {} }).recipe, undefined);
  assert.equal(packageCommandOptional({ winPackage: {} }, "tauri"), true);
  assert.equal(packageCommandOptional({}, "tauri"), false);
});

test("stage-sidecar CLI arguments", () => {
  assert.deepEqual(parseStageSidecarArgs(["--target", "aarch64-apple-darwin", "--json"]), { config: "right-release.config.mjs", target: "aarch64-apple-darwin", json: true });
  assert.throws(() => parseStageSidecarArgs(["--target"]), /needs a value/);
  assert.throws(() => parseStageSidecarArgs(["--bogus"]), /unknown/);
});

// ---- profile, tauriConfig, prepare and postBuild (bogusyogi/rightkit#345) ----

const planOf = (command) => {
  assert.equal(command.cmd, process.execPath);
  assert.ok(path.isAbsolute(command.args[0]) && command.args[0].endsWith("recipe-build.mjs"));
  assert.equal(command.args[1], "--plan");
  return JSON.parse(command.args[2]);
};

test("winPackage profile and tauriConfig reach both Tauri phases; prepare wraps prePackage in recipe-build", () => {
  const direct = expandWinPackageTarget(winTarget({ winPackage: { installer: "nsis", productName: "Demo", updater: true, profile: "release-iterate", tauriConfig: "src-tauri/tauri.bundle.json" } }), { appRoot, config: {} });
  assert.equal(direct.prePackage.cmd, "pnpm");
  assert.deepEqual(direct.prePackage.args.slice(-5), ["--config", "src-tauri/tauri.bundle.json", "--", "--profile", "release-iterate"]);
  assert.deepEqual(direct.package.args.slice(-2), ["--profile", "release-iterate"]);
  assert.ok(direct.package.args.includes("src-tauri/tauri.bundle.json"));
  assert.equal(JSON.parse(direct.package.args[direct.package.args.indexOf("--profile") - 2]).bundle.createUpdaterArtifacts, false);
  assert.equal(direct.winPackage, undefined);

  const prepare = [{ cmd: "node", args: ["scripts/stage-ffmpeg.mjs"] }, { cmd: "right-release", args: ["stage-sidecar"], cwd: "." }];
  const wrapped = expandWinPackageTarget(winTarget({ winPackage: { installer: "nsis", productName: "Demo", updater: true, prepare } }), { appRoot, config: {} });
  const steps = planOf(wrapped.prePackage).steps;
  assert.deepEqual(steps.map((step) => step.phase), ["prepare", "prepare", "build"]);
  assert.deepEqual(steps[0], { phase: "prepare", cmd: "node", args: ["scripts/stage-ffmpeg.mjs"] });
  assert.deepEqual(steps[2].args.slice(0, 4), ["exec", "tauri", "build", "--no-bundle"]);
  assert.equal(wrapped.prePackage.env.TAURI_SIGNING_PRIVATE_KEY, "");
  assert.equal(wrapped.package.cmd, "pnpm");
});

test("winPackage refuses postBuild, absolute or escaping paths, bad profiles and malformed commands", () => {
  const cfg = { appRoot, config: {} };
  const withSpec = (extra) => winTarget({ winPackage: { installer: "nsis", productName: "Demo", updater: true, ...extra } });
  assert.throws(() => expandWinPackageTarget(withSpec({ postBuild: [{ cmd: "x" }] }), cfg), /postBuild is not supported/);
  assert.throws(() => expandWinPackageTarget(withSpec({ tauriConfig: "../x.json" }), cfg), /stay inside the app/);
  assert.throws(() => expandWinPackageTarget(withSpec({ tauriConfig: "cfg.toml" }), cfg), /\.json/);
  assert.throws(() => expandWinPackageTarget(withSpec({ profile: "Release Fast" }), cfg), /Cargo profile/);
  assert.throws(() => expandWinPackageTarget(withSpec({ prepare: "node x" }), cfg), /array/);
  assert.throws(() => expandWinPackageTarget(withSpec({ prepare: [{ cmd: "node", shell: true }] }), cfg), /unknown key/);
  assert.throws(() => expandWinPackageTarget(withSpec({ prepare: [{ cmd: "node", args: [1] }] }), cfg), /list of strings/);
  assert.throws(() => expandWinPackageTarget(withSpec({ prepare: [{ cmd: "node", cwd: "/abs" }] }), cfg), /stay inside the app/);
});

test("macPackage profile, tauriConfig, prepare and postBuild run in order through one prePackage command", () => {
  const out = expandMacPackageTarget(macTarget({ macPackage: {
    profile: "release-iterate", tauriConfig: "src-tauri/tauri.mac.json", features: ["qa"],
    prepare: [{ cmd: "node", args: ["scripts/stage-pdfium.mjs"] }],
    postBuild: [{ cmd: "ditto", args: ["--noqtn", "libpdfium.dylib", "Demo.app/Contents/Frameworks/libpdfium.dylib"] }],
  } }), { appRoot });
  const steps = planOf(out.prePackage).steps;
  assert.deepEqual(steps.map((step) => step.phase), ["prepare", "build", "postBuild"]);
  const build = steps[1].args;
  assert.deepEqual(build.slice(-3), ["--", "--profile", "release-iterate"]);
  assert.ok(build.includes("--no-sign") && build.includes("universal-apple-darwin"));
  assert.equal(build[build.indexOf("--config") + 1], "src-tauri/tauri.mac.json");
  assert.ok(build.indexOf("--features") < build.indexOf("--"), "Cargo flags go after --, Tauri flags before");
  assert.deepEqual(steps[2], { phase: "postBuild", cmd: "ditto", args: ["--noqtn", "libpdfium.dylib", "Demo.app/Contents/Frameworks/libpdfium.dylib"] });
  assert.deepEqual(out.sign.prePackageFiles, [out.notarize.app]);
  assert.equal(out.notarize.appBeforePackage, true);
  // Without prepare/postBuild the direct Tauri command is kept, now with the profile.
  const plain = expandMacPackageTarget(macTarget({ macPackage: { profile: "release-iterate" } }), { appRoot });
  assert.equal(plain.prePackage.cmd, "pnpm");
  assert.deepEqual(plain.prePackage.args.slice(-3), ["--", "--profile", "release-iterate"]);
  assert.throws(() => expandMacPackageTarget(macTarget({ macPackage: { postBuild: [{ args: [] }] } }), { appRoot }), /cmd must be/);
  assert.throws(() => expandMacPackageTarget(macTarget({ macPackage: { profile: "X" } }), { appRoot }), /Cargo profile/);
});
