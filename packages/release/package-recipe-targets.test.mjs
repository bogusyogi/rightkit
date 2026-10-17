import assert from "node:assert/strict";
import path from "node:path";
import test from "node:test";
import { assertNoForbiddenBuildFeatures, buildFeatureIssues } from "./qa-build-features.mjs";
import { expandRecipeBuildCommands } from "./recipe-build.mjs";
import { commandTokens } from "./release-switch-scan.mjs";
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
  // The build always runs through recipe-build now: stale cleanup first, the stability poll right after the build.
  const plan = planOf(out.prePackage);
  assert.deepEqual(plan.steps.map((step) => step.phase), ["build"]);
  assert.deepEqual(plan.steps[0].args.slice(0, 4), ["exec", "tauri", "build", "--no-bundle"]);
  assert.deepEqual(plan.cleanup, { remove: ["src-tauri/target/x86_64-pc-windows-msvc/release/demo.exe", "src-tauri/target/x86_64-pc-windows-msvc/release/bundle/nsis/Demo_1.0.0_x64-setup.exe"] });
  assert.deepEqual(plan.stability, { files: ["src-tauri/target/x86_64-pc-windows-msvc/release/demo.exe"] });
  assert.equal(out.prePackage.timeoutMs, 20 * 60 * 1000);
  assert.equal(out.package.timeoutMs, 20 * 60 * 1000);
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
  assert.deepEqual(parseStageSidecarArgs(["--target", "aarch64-apple-darwin", "--json"]), { config: "right-release.config.mjs", target: "aarch64-apple-darwin", json: true, check: false });
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
  assert.deepEqual(planOf(direct.prePackage).steps[0].args.slice(-5), ["--config", "src-tauri/tauri.bundle.json", "--", "--profile", "release-iterate"]);
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

// ---- triple, cleanup, jobs, timeout, cleanStale, managed target roots (bogusyogi/rightkit#345 gap reports) ----

test("macPackage triple overrides universal; universal false is this host's arch", () => {
  const app = "src-tauri/target/aarch64-apple-darwin/release/bundle/macos/Demo.app";
  const out = expandMacPackageTarget(macTarget({ macPackage: { triple: "aarch64-apple-darwin", universal: true }, notarize: { app, file: "dist/Demo_arm64.dmg" } }), { appRoot, env: {} });
  assert.equal(out.prePackage.args[out.prePackage.args.indexOf("--target") + 1], "aarch64-apple-darwin");
  assert.equal(out.recipe.universal, false);
  const host = expandMacPackageTarget(macTarget({ macPackage: { universal: false } }), { appRoot, env: {} });
  assert.equal(host.prePackage.args[host.prePackage.args.indexOf("--target") + 1], process.arch === "arm64" ? "aarch64-apple-darwin" : "x86_64-apple-darwin");
  assert.throws(() => expandMacPackageTarget(macTarget({ macPackage: { triple: "universal-apple-darwin" } }), { appRoot, env: {} }), /triple must be one of/);
  assert.throws(() => expandMacPackageTarget(macTarget({ macPackage: { triple: "x86_64-apple-darwin" } }), { appRoot, env: {} }), /notarize.app is under universal-apple-darwin/);
});

test("macPackage cleanup becomes built-in recipe-build cleanup, not an app script", () => {
  const out = expandMacPackageTarget(macTarget({ dmg: { volname: "Demo Installer" }, macPackage: { cleanup: { staleApps: true, staleDmgVolumes: true, lsclean: true } } }), { appRoot, env: {} });
  const plan = planOf(out.prePackage);
  assert.deepEqual(plan.steps.map((step) => step.phase), ["build"]);
  assert.deepEqual(plan.cleanup, { dmgVolumes: ["Demo", "Demo Installer"], apps: { dir: "src-tauri/target/universal-apple-darwin/release/bundle/macos", remove: true, unregister: true } });
  const lsOnly = planOf(expandMacPackageTarget(macTarget({ macPackage: { cleanup: { lsclean: true } } }), { appRoot, env: {} }).prePackage);
  assert.deepEqual(lsOnly.cleanup.apps, { dir: "src-tauri/target/universal-apple-darwin/release/bundle/macos", remove: false, unregister: true });
  assert.equal(lsOnly.cleanup.dmgVolumes, undefined);
  assert.equal(expandMacPackageTarget(macTarget({ macPackage: { cleanup: {} } }), { appRoot, env: {} }).prePackage.cmd, "pnpm");
  assert.throws(() => expandMacPackageTarget(macTarget({ macPackage: { cleanup: { shell: true } } }), { appRoot, env: {} }), /unknown key/);
  assert.throws(() => expandMacPackageTarget(macTarget({ macPackage: { cleanup: { lsclean: "yes" } } }), { appRoot, env: {} }), /must be a boolean/);
});

test("winPackage jobs, timeoutMs and cleanStale", () => {
  const spec = (extra) => winTarget({ winPackage: { installer: "nsis", productName: "Demo", updater: true, ...extra } });
  const cfg = { appRoot, config: {}, env: {} };
  const out = expandWinPackageTarget(spec({ jobs: 4, profile: "release-iterate", timeoutMs: 600000, cleanStale: false }), cfg);
  const plan = planOf(out.prePackage);
  assert.deepEqual(plan.steps[0].args.slice(-5), ["--", "--profile", "release-iterate", "--jobs", "4"]);
  assert.equal(plan.cleanup, undefined);
  assert.deepEqual(plan.stability.files, out.sign.prePackageFiles);
  assert.equal(out.prePackage.timeoutMs, 600000);
  assert.equal(out.package.timeoutMs, 600000);
  assert.deepEqual(planOf(expandWinPackageTarget(spec({ jobs: 2 }), cfg).prePackage).steps[0].args.slice(-3), ["--", "--jobs", "2"]);
  assert.throws(() => expandWinPackageTarget(spec({ jobs: 0 }), cfg), /jobs must be a positive integer/);
  assert.throws(() => expandWinPackageTarget(spec({ jobs: 1.5 }), cfg), /jobs/);
  assert.throws(() => expandWinPackageTarget(spec({ timeoutMs: -1 }), cfg), /timeoutMs/);
  assert.throws(() => expandWinPackageTarget(spec({ cleanStale: "yes" }), cfg), /cleanStale must be a boolean/);
});

test("managed Cargo target roots: declared paths follow cargo metadata when requested or CARGO_TARGET_DIR is set", () => {
  const managed = path.join(appRoot, ".cargo-target");
  const asked = [];
  const resolveTargetRoot = (manifest) => { asked.push(manifest); return managed; };
  const win = expandWinPackageTarget(winTarget({ winPackage: { installer: "nsis", productName: "Demo", updater: true, cargoTargetRoot: true } }), { appRoot, config: {}, env: {}, resolveTargetRoot });
  assert.deepEqual(asked, [path.join(appRoot, "src-tauri", "Cargo.toml")]);
  assert.deepEqual(win.sign.prePackageFiles, [".cargo-target/x86_64-pc-windows-msvc/release/demo.exe"]);
  assert.deepEqual(win.sign.files, [".cargo-target/x86_64-pc-windows-msvc/release/bundle/nsis/Demo_1.0.0_x64-setup.exe"]);
  assert.deepEqual(planOf(win.prePackage).stability.files, win.sign.prePackageFiles);
  assert.equal(win.winPackage, undefined);

  // An explicit Cargo.toml path; the mac app, signing set and bundle dir move together. (CARGO_TARGET_DIR alone triggers the
  // same lookup only when <tauriCwd>/src-tauri/Cargo.toml exists, which the fixture app does not have.)
  const mac = expandMacPackageTarget(macTarget({ macPackage: { cargoTargetRoot: "src-tauri/Cargo.toml", cleanup: { staleApps: true } } }), { appRoot, env: { CARGO_TARGET_DIR: managed }, resolveTargetRoot });
  assert.equal(mac.notarize.app, ".cargo-target/universal-apple-darwin/release/bundle/macos/Demo.app");
  assert.deepEqual(mac.sign.prePackageFiles, [mac.notarize.app]);
  assert.equal(planOf(mac.prePackage).cleanup.apps.dir, ".cargo-target/universal-apple-darwin/release/bundle/macos");

  // Not requested and no env: untouched and cargo is never asked. A path outside the default prefix is left alone.
  asked.length = 0;
  assert.equal(expandWinPackageTarget(winTarget(), { appRoot, config: {}, env: {}, resolveTargetRoot }).sign.prePackageFiles[0], "src-tauri/target/x86_64-pc-windows-msvc/release/demo.exe");
  assert.deepEqual(asked, []);
  const custom = expandWinPackageTarget(winTarget({ sign: { prePackageFiles: ["out/demo.exe"], files: ["out/setup.exe"] }, winPackage: { installer: "nsis", productName: "Demo", updater: true, cargoTargetRoot: true } }), { appRoot, config: {}, env: {}, resolveTargetRoot });
  assert.deepEqual(custom.sign.prePackageFiles, ["out/demo.exe"]);

  // A target directory outside the app cannot be addressed by app-relative paths: fail closed. A resolver failure too.
  const outside = () => path.resolve(appRoot, "..", "elsewhere");
  assert.throws(() => expandWinPackageTarget(winTarget({ winPackage: { installer: "nsis", productName: "Demo", updater: true, cargoTargetRoot: true } }), { appRoot, config: {}, env: {}, resolveTargetRoot: outside }), /outside the app/);
  assert.throws(() => expandMacPackageTarget(macTarget({ macPackage: { cargoTargetRoot: true } }), { appRoot, env: {}, resolveTargetRoot: () => { throw new Error("no cargo"); } }), /cannot resolve the Cargo target directory/);
  assert.throws(() => expandMacPackageTarget(macTarget({ macPackage: { cargoTargetRoot: 5 } }), { appRoot, env: {}, resolveTargetRoot }), /cargoTargetRoot/);
});

test("F1: a recipe-build wrapped macPackage with features [qa-native] is refused by the forbidden-feature scan exactly as an unwrapped one", () => {
  const wrapped = expandMacPackageTarget(macTarget({ macPackage: { features: ["qa-native"], cleanup: { staleApps: true } } }), { appRoot, env: {} });
  const plain = expandMacPackageTarget(macTarget({ macPackage: { features: ["qa-native"] } }), { appRoot, env: {} });
  assert.equal(plain.prePackage.cmd, "pnpm");
  assert.match(wrapped.prePackage.args[1], /--plan/);
  const scan = (prePackage) => buildFeatureIssues({ root: appRoot, commands: [{ label: "target.prePackage", command: prePackage }] }).filter((issue) => issue.includes("qa-native"));
  assert.ok(scan(plain.prePackage).length > 0, "the unwrapped command is refused");
  assert.ok(scan(wrapped.prePackage).length > 0, "the wrapped command is refused too");
  assert.throws(() => assertNoForbiddenBuildFeatures({ root: appRoot, commands: [{ label: "target.prePackage", command: wrapped.prePackage }] }), /qa-native/);
  // The closure and token scan inputs carry every step, so `--features qa-native` is a plain token pair there.
  const commands = expandRecipeBuildCommands(wrapped.prePackage, { root: appRoot });
  assert.ok(commands.length >= 2);
  const featureIndex = commandTokens(commands.at(-1)).indexOf("--features");
  assert.ok(featureIndex >= 0 && commandTokens(commands.at(-1))[featureIndex + 1] === "qa-native");
});

test("F1: prepare and postBuild steps and --plan-file plans are scanned, and an unreadable plan fails closed", () => {
  const prePackage = { cmd: process.execPath, args: ["/x/recipe-build.mjs", "--plan", JSON.stringify({ version: 1, steps: [{ phase: "prepare", cmd: "cargo", args: ["build", "-F", "dev-native"] }, { phase: "build", cmd: "pnpm", args: ["exec", "tauri", "build"] }, { phase: "postBuild", cmd: "cargo", args: ["run", "--all-features"] }] })] };
  const found = buildFeatureIssues({ root: appRoot, commands: [{ label: "target.prePackage", command: prePackage }] });
  assert.ok(found.some((issue) => issue.includes("dev-native")));
  assert.ok(found.some((issue) => issue.includes("--all-features")));
  const planFile = { cmd: "node", args: ["recipe-build.mjs", "--plan-file", "plan.json"] };
  const readFile = () => JSON.stringify({ version: 1, steps: [{ phase: "build", cmd: "cargo", args: ["build", "--features=qa-control"] }] });
  assert.equal(expandRecipeBuildCommands(planFile, { root: appRoot, readFile }).length, 2);
  assert.throws(() => expandRecipeBuildCommands(planFile, { root: appRoot, readFile: () => { throw new Error("ENOENT"); } }), { code: "RECIPE_BUILD_PLAN_INVALID" });
  assert.throws(() => expandRecipeBuildCommands({ cmd: "node", args: ["recipe-build.mjs", "--plan", "{not json"] }), { code: "RECIPE_BUILD_PLAN_INVALID" });
  assert.deepEqual(expandRecipeBuildCommands({ cmd: "pnpm", args: ["tauri", "build"] }), [{ cmd: "pnpm", args: ["tauri", "build"] }]);
});
