// Item 7 row 7.04c (decision 3, C2): the Cargo target in a door split. The snapshot holds no `src-tauri/target`, so a declared artifact path means the
// target the split bound for that manifest (R-T1), through ONE resolver (R-T2), read and signed in place (R-T3), with no link and no copy (R-T5),
// and no target-relative path in a command RightKit does not remap (R-T4). Test names carry the decision's row ids: T1-T7 and N1-N10.
//
// Evidence labels (see also the report): A is a unit of the resolver, the mapper and the scan: every host. B drives the real `sealRelease` function
// (extracted from build-release.mjs, as build-release.test.mjs does). C starts the REAL RightKit subcommands (create-mac-updater, sign-updater,
// mirror-root-artifact) as split children. D starts the REAL `build-release.mjs` as a split child under a live door run (a real run file, a real
// snapshot) and proves the refusals come before any build tool starts. The real-cargo L row (T2) is door-real-cargo.test.mjs's sibling below, Linux only.
// The dev-broker rows (T3, T4, T5 broker, N8 broker, N6 broker) need the snapshot carrier (rows 7.20 to 7.24) and RustRight's item-4 dev pins: BLOCKED.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { runInNewContext } from "node:vm";
import { spawnSync } from "./background-process.mjs";
import { resolveNativeCargoLayout } from "./native-cargo-layout.mjs";
import { expandMacPackageTarget, expandWinPackageTarget } from "./package-recipe-targets.mjs";
import { collectPreflight } from "./preflight.mjs";
import {
  ArtifactPathError, SPLIT_TARGET_DRIFT_CODE, SPLIT_TARGET_PATH_CODE, SPLIT_TARGET_UNRESOLVED_CODE, assertSplitCommandPaths, boundTargetsEvidence,
  crossCheckBoundTargets, findTargetPrefixedPath, isRightKitEntry, releaseCommandSteps, resolveArtifactPath, resolveArtifactWritePath, splitArtifactContext,
} from "./release-artifact-path.mjs";
import { runSignUpdater } from "./sign-updater.mjs";
import { fixture, git as fixtureGit, managedEnv } from "./test-support/fixture.mjs";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const LINKS = process.platform === "win32" ? "creating links needs a privilege on Windows (the link rows run on the hosts that can)" : false;

function tmp(t, prefix = "rk-art-") {
  const dir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), prefix)));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  return dir;
}
const put = (base, rel, body = "x") => { fs.mkdirSync(path.dirname(path.join(base, rel)), { recursive: true }); fs.writeFileSync(path.join(base, rel), body); return path.join(base, rel); };
const MANIFEST = '[package]\nname = "app"\nversion = "0.1.0"\nedition = "2021"\n';

const LAYOUTS = {
  "src-tauri (the default, most apps)": { manifest: "src-tauri/Cargo.toml", prefix: "src-tauri/target/" },
  "root target/ (genright)": { manifest: "Cargo.toml", nativeAssembly: { cargoManifest: "Cargo.toml" }, prefix: "target/" },
  "nested tauri-app-next/src-tauri/target/ (heardright)": { manifest: "tauri-app-next/src-tauri/Cargo.toml", nativeAssembly: { cargoManifest: "tauri-app-next/src-tauri/Cargo.toml" }, prefix: "tauri-app-next/src-tauri/target/" },
  "hub/src-tauri/target/ (pulse)": { manifest: "hub/src-tauri/Cargo.toml", nativeAssembly: { cargoManifest: "hub/src-tauri/Cargo.toml" }, prefix: "hub/src-tauri/target/" },
};

/** A snapshot app root, an editable checkout, a managed artifact directory and a bound target outside the snapshot (or, `boundIn: "checkout"`, the checkout's own). */
function world(t, { layout = LAYOUTS["src-tauri (the default, most apps)"], boundIn = "outside", target = null } = {}) {
  const root = tmp(t);
  const snapshot = path.join(root, "snapshot"); const checkout = path.join(root, "checkout"); const artifactDir = path.join(root, "managed-artifacts");
  // A crate with a target, so a REAL `cargo metadata` (CI runs and the Linux row) answers for it as the stub does elsewhere.
  for (const base of [snapshot, checkout]) { put(base, layout.manifest, MANIFEST); put(base, path.posix.join(path.posix.dirname(layout.manifest), "src/main.rs"), "fn main() {}\n"); }
  fs.mkdirSync(artifactDir);
  const bound = boundIn === "checkout" ? path.join(checkout, path.dirname(layout.manifest), "target") : path.join(root, "bound-target");
  fs.mkdirSync(bound, { recursive: true });
  const config = layout.nativeAssembly ? { nativeAssembly: layout.nativeAssembly } : {};
  const nativeLayout = resolveNativeCargoLayout({ appRoot: snapshot, config });
  const calls = [];
  const make = ({ env = {}, resolveTargetRoot = (manifest) => { calls.push(manifest); return bound; }, ...rest } = {}) => splitArtifactContext({
    cwd: snapshot, env: { RK_GATE_CHECKOUT: checkout, RIGHT_RELEASE_ARTIFACT_DIR: artifactDir, ...env }, nativeLayout, config, target, snapshotRoot: snapshot, resolveTargetRoot, ...rest,
  });
  return { root, snapshot, checkout, artifactDir, bound, nativeLayout, config, calls, make, ctx: make(), prefix: layout.prefix };
}

const refusal = (code, pattern) => (error) => error instanceof ArtifactPathError && error.code === code && (!pattern || pattern.test(error.message));

// ---- A. the resolver ---------------------------------------------------------------------------------------------------------------

for (const [name, layout] of Object.entries(LAYOUTS)) {
  test(`[T1] resolver table, ${name}: the prefix maps to the bound target; an absolute path under it passes; a non-target relative path is the snapshot's, then the artifact directory's`, (t) => {
    const w = world(t, { layout });
    const bound = w.ctx.boundTargets.find((entry) => entry.prefix === layout.prefix);
    assert.ok(bound, `a bound target for ${layout.prefix}: ${JSON.stringify(w.ctx.boundTargets.map((entry) => entry.prefix))}`);
    assert.equal(bound.dir, w.bound);
    assert.equal(bound.manifest, path.join(w.snapshot, ...layout.manifest.split("/")));
    assert.equal(w.ctx.mode, "split");
    const rest = "release/bundle/macos/App.app";
    for (const spelled of [`${layout.prefix}${rest}`, `./${layout.prefix}${rest}`, `${layout.prefix}${rest}`.replaceAll("/", "\\"), layout.prefix.replace(/\/$/, "")]) {
      const expected = spelled === layout.prefix.replace(/\/$/, "") ? w.bound : path.join(w.bound, ...rest.split("/"));
      assert.equal(resolveArtifactPath(spelled, w.ctx), expected, spelled);
    }
    assert.equal(resolveArtifactPath(path.join(w.bound, "release", "x.exe"), w.ctx), path.join(w.bound, "release", "x.exe"), "an absolute path inside the bound target");
    // A non-target relative path: the snapshot app root first, then the managed artifact directory, else the snapshot path (for a writer or the message).
    assert.equal(resolveArtifactPath("App.dmg", w.ctx), path.join(w.snapshot, "App.dmg"), "nothing anywhere: the snapshot path");
    put(w.artifactDir, "App.dmg");
    assert.equal(resolveArtifactPath("App.dmg", w.ctx), path.join(w.artifactDir, "App.dmg"), "only in the artifact directory");
    put(w.snapshot, "App.dmg");
    assert.equal(resolveArtifactPath("App.dmg", w.ctx), path.join(w.snapshot, "App.dmg"), "the snapshot wins");
    assert.equal(resolveArtifactPath("sub/dir/Other.dmg", w.ctx), path.join(w.snapshot, "sub", "dir", "Other.dmg"));
    // The artifact directory is also an accepted absolute location.
    assert.equal(resolveArtifactPath(path.join(w.artifactDir, "App.dmg"), w.ctx), path.join(w.artifactDir, "App.dmg"));
  });
}

test("[T1] the bound target is resolved once per manifest, at the start, through the published resolver (not per path)", (t) => {
  const w = world(t);
  assert.deepEqual(w.calls, [path.join(w.snapshot, "src-tauri", "Cargo.toml")], "one cargo metadata call at context creation");
  for (let i = 0; i < 5; i += 1) resolveArtifactPath("src-tauri/target/release/x", w.ctx);
  assert.equal(w.calls.length, 1);
  assert.deepEqual(boundTargetsEvidence(w.ctx), [{ prefix: "src-tauri/target/", manifest: "src-tauri/Cargo.toml", dir: w.bound }], "what the sealed manifest records");
});

test("[T1/N10] checkout mode is the historical rule byte for byte: the app root, then RIGHT_RELEASE_ARTIFACT_DIR/<basename>, else the app root; nothing is bound", (t) => {
  const root = tmp(t); const app = path.join(root, "app"); const managed = path.join(root, "managed");
  fs.mkdirSync(app); fs.mkdirSync(managed);
  const env = { RIGHT_RELEASE_ARTIFACT_DIR: managed };
  const ctx = splitArtifactContext({ cwd: app, env, nativeLayout: null, resolveTargetRoot: () => { throw new Error("checkout mode asks no Cargo"); } });
  assert.equal(ctx.mode, "checkout"); assert.deepEqual(boundTargetsEvidence(ctx), []);
  const historical = (declared) => { const local = path.resolve(app, declared); if (fs.existsSync(local)) return local; const candidate = path.join(path.resolve(env.RIGHT_RELEASE_ARTIFACT_DIR), path.basename(declared)); return fs.existsSync(candidate) ? candidate : local; };
  for (const declared of ["src-tauri/target/release/x", "App.dmg", "a/b/App.dmg", "../outside.bin"]) assert.equal(resolveArtifactPath(declared, ctx), historical(declared), `${declared}: neither exists`);
  put(managed, "App.dmg"); put(managed, "x");
  for (const declared of ["App.dmg", "a/b/App.dmg", "src-tauri/target/release/x"]) assert.equal(resolveArtifactPath(declared, ctx), historical(declared), `${declared}: only the artifact directory has it`);
  put(app, "App.dmg");
  assert.equal(resolveArtifactPath("App.dmg", ctx), historical("App.dmg"));
  assert.equal(resolveArtifactPath("..\\outside.bin", ctx), historical("..\\outside.bin"), "checkout mode accepts what it always accepted (no new refusal)");
  assert.equal(resolveArtifactWritePath("out/App.dmg", ctx), path.resolve(app, "out/App.dmg"));
  assert.equal(findTargetPrefixedPath("x", []), null);
  assert.doesNotThrow(() => assertSplitCommandPaths([{ cmd: "node", args: ["x.mjs", "src-tauri/target/release/App"] }], ctx), "the command scan is a split rule: checkout mode checks nothing");
});

test("[N2] an absolute path into the editable checkout (other than its bound target), or into an unrelated directory, is RK_SPLIT_TARGET_PATH", (t) => {
  const w = world(t);
  for (const bad of [path.join(w.checkout, "src-tauri", "target", "release", "x"), path.join(w.checkout, "App.dmg"), path.join(w.root, "elsewhere", "x"), path.join(w.snapshot, "src-tauri", "target", "x")]) {
    assert.throws(() => resolveArtifactPath(bad, w.ctx), refusal(SPLIT_TARGET_PATH_CODE, /neither inside a bound Cargo target .* nor inside RIGHT_RELEASE_ARTIFACT_DIR/), bad);
  }
  // On Linux the bound target IS inside the editable checkout: that directory is accepted, its siblings and a look-alike prefix are not.
  const l = world(t, { boundIn: "checkout" });
  assert.equal(resolveArtifactPath(path.join(l.bound, "release", "x"), l.ctx), path.join(l.bound, "release", "x"));
  for (const bad of [path.join(l.checkout, "src-tauri", "other"), path.join(l.checkout, "src-tauri", "target-evil", "x"), path.join(l.checkout, "README.md"), l.checkout]) {
    assert.throws(() => resolveArtifactPath(bad, l.ctx), refusal(SPLIT_TARGET_PATH_CODE), bad);
  }
});

test("[N3] a relative path with a .. segment is refused, in every spelling", (t) => {
  const w = world(t);
  for (const bad of ["../x", "a/../b", "src-tauri/target/../../x", "..\\x", "./../x", "src-tauri\\target\\..\\x"]) assert.throws(() => resolveArtifactPath(bad, w.ctx), refusal(SPLIT_TARGET_PATH_CODE, /\.\. segment/), bad);
  assert.throws(() => resolveArtifactWritePath("a/../b", w.ctx), refusal(SPLIT_TARGET_PATH_CODE));
  assert.doesNotThrow(() => resolveArtifactPath("a..b/c..d", w.ctx), "a dot-dot inside a name is not a segment");
});

test("[N4] a stale DMG in the editable checkout is never the answer: the resolver never returns a checkout path, a link into it is refused, the artifact directory is not read for a writer", { skip: false }, (t) => {
  const w = world(t);
  put(w.checkout, "App.dmg", "STALE");
  const found = resolveArtifactPath("App.dmg", w.ctx);
  assert.equal(found, path.join(w.snapshot, "App.dmg"));
  assert.equal(fs.existsSync(found), false, "the caller then reports a missing artifact");
  assert.ok(!found.startsWith(w.checkout), "never the checkout");
  put(w.artifactDir, "App.dmg", "FROM-ARTIFACT-DIR");
  assert.equal(resolveArtifactPath("App.dmg", w.ctx), path.join(w.artifactDir, "App.dmg"));
  assert.equal(resolveArtifactWritePath("App.dmg", w.ctx), path.join(w.snapshot, "App.dmg"), "a writer never lands in a stale artifact-directory copy");
});

test("[N4] a link in the snapshot that resolves into the checkout is refused, not followed", { skip: LINKS }, (t) => {
  const w = world(t);
  put(w.checkout, "App.dmg", "STALE");
  fs.symlinkSync(path.join(w.checkout, "App.dmg"), path.join(w.snapshot, "App.dmg"));
  assert.throws(() => resolveArtifactPath("App.dmg", w.ctx), refusal(SPLIT_TARGET_PATH_CODE, /resolves outside the snapshot/));
});

// ---- A. the bound target (R-T1) -----------------------------------------------------------------------------------------------------

test("[N8] a manifest whose target cannot be resolved is RK_SPLIT_TARGET_UNRESOLVED: no snapshot-relative fallback, no path is ever resolved", (t) => {
  const w = world(t);
  assert.throws(() => w.make({ resolveTargetRoot: () => { throw new Error("the carrier refuses cargo metadata for a split child"); } }), refusal(SPLIT_TARGET_UNRESOLVED_CODE, /never falls back to a snapshot-relative target\//));
  assert.throws(() => w.make({ resolveTargetRoot: () => path.join(w.snapshot, "src-tauri", "target") }), refusal(SPLIT_TARGET_UNRESOLVED_CODE, /resolves inside the snapshot/), "cargo's own answer for an unpinned split: inside the snapshot");
  assert.throws(() => w.make({ resolveTargetRoot: () => "target" }), refusal(SPLIT_TARGET_UNRESOLVED_CODE, /absolute/));
  fs.rmSync(path.join(w.snapshot, "src-tauri", "Cargo.toml"));
  assert.throws(() => w.make(), refusal(SPLIT_TARGET_UNRESOLVED_CODE, /not in the snapshot/));
});

test("[N6] a target that differs from the CARGO_TARGET_DIR the gate bound is RK_SPLIT_TARGET_DRIFT; the door's cross-check refuses a recorded target that is not its own", (t) => {
  const w = world(t);
  const other = path.join(w.root, "another-generation");
  assert.throws(() => w.make({ env: { CARGO_TARGET_DIR: other } }), refusal(SPLIT_TARGET_DRIFT_CODE, /not the CARGO_TARGET_DIR the gate bound/));
  assert.doesNotThrow(() => w.make({ env: { CARGO_TARGET_DIR: w.bound } }), "control: equal to the gate's");
  // The door's side: the child's recorded value is never trusted alone.
  assert.deepEqual(crossCheckBoundTargets({ recorded: boundTargetsEvidence(w.ctx), expected: [w.bound] }), []);
  const drift = crossCheckBoundTargets({ recorded: boundTargetsEvidence(w.ctx), expected: [other] });
  assert.equal(drift.length, 1); assert.match(drift[0], /boundTargets\[0\] is .*bound-target, not the directory the gate bound/);
  assert.match(crossCheckBoundTargets({ recorded: undefined, expected: [w.bound] })[0], /records no bound Cargo target/);
  assert.match(crossCheckBoundTargets({ recorded: [{ prefix: "x/", manifest: "m", dir: null }], expected: [w.bound] })[0], /records no directory/);
  assert.deepEqual(crossCheckBoundTargets({ recorded: [], expected: [] }), [], "no Cargo workspace, nothing to cross-check");
  if (process.platform !== "win32") {
    const via = path.join(w.root, "via-link"); fs.symlinkSync(w.bound, via);
    assert.deepEqual(crossCheckBoundTargets({ recorded: [{ dir: via }], expected: [w.bound] }), [], "equal by realpath");
  }
});

// ---- A. R-T4: the command scan ------------------------------------------------------------------------------------------------------

test("[N1] a step the app owns that names a target-relative path is RK_SPLIT_TARGET_PATH: argv, --x=, JSON-embedded, env, ./, backslashes, after : and ,", (t) => {
  const w = world(t);
  const refused = (command) => assert.throws(() => assertSplitCommandPaths([{ name: "postSign", ...command }], w.ctx), refusal(SPLIT_TARGET_PATH_CODE, /postSign \(.*\) names the target-relative path src-tauri\/target\//), JSON.stringify(command));
  refused({ cmd: "node", args: ["scripts/verify.mjs", "--app", "src-tauri/target/release/bundle/macos/App.app"] });
  refused({ cmd: "node", args: ["scripts/verify.mjs", "--app=src-tauri/target/release/App"] });
  refused({ cmd: "node", args: ["x.mjs", JSON.stringify({ app: "src-tauri/target/release/App.app" })] });
  refused({ cmd: "node", args: ["x.mjs", "./src-tauri/target/release/App"] });
  refused({ cmd: "node", args: ["x.mjs", "src-tauri\\target\\release\\App.exe"] });
  refused({ cmd: "node", args: ["x.mjs", "a:src-tauri/target/release/App,b"] });
  refused({ cmd: "node", args: ["x.mjs"], env: { APP: "src-tauri/target/release/App" } });
  refused({ cmd: "sh", args: ["-c", "cp src-tauri/target/release/App /tmp"] });
  refused({ cmd: "src-tauri/target/release/tool" });
  // Not a target-relative path: absolute (postright and cutright), a longer directory name, the word inside a name.
  for (const fine of [["x.mjs", path.join(w.bound, "release", "App")], ["x.mjs", "/Users/a/src-tauri/target/release/App"], ["x.mjs", "src-tauri/target-evil/App"], ["x.mjs", "my-src-tauri/target/App"], ["x.mjs", "src-tauri/targets"], ["x.mjs", "src-tauri/Cargo.toml"]]) {
    assert.doesNotThrow(() => assertSplitCommandPaths([{ cmd: "node", args: fine }], w.ctx), fine.join(" "));
  }
  assert.deepEqual(w.ctx.prefixes, ["src-tauri/target/"]);
});

test("[N1] every RightKit entry is exempt (it remaps), and a look-alike is not: right-release, pnpm exec right-release, rightkit-qa, this package's own scripts", (t) => {
  const w = world(t);
  const own = path.join(HERE, "create-mac-updater.mjs");
  const path1 = "src-tauri/target/release/bundle/macos/App.app";
  for (const exempt of [{ cmd: "right-release", args: ["x", "--app", path1] }, { cmd: "pnpm", args: ["exec", "right-release", "x", path1] }, { cmd: "rightkit-qa", args: ["run", path1] }, { cmd: process.execPath, args: [own, "--app", path1, "--output", "App.app.tar.gz"] }, { cmd: process.execPath, args: [path.join(HERE, "recipe-build.mjs"), "--plan", JSON.stringify({ x: path1 })] }]) {
    assert.equal(isRightKitEntry(exempt), true, JSON.stringify(exempt));
    assert.doesNotThrow(() => assertSplitCommandPaths([exempt], w.ctx));
  }
  for (const spoof of [{ cmd: process.execPath, args: [path.join(w.root, "evil", "create-mac-updater.mjs"), path1] }, { cmd: "node", args: ["scripts/right-release.mjs", path1] }, { cmd: "pnpm", args: ["exec", "tauri", "signer", path1] }]) {
    assert.equal(isRightKitEntry(spoof), false, JSON.stringify(spoof));
    assert.throws(() => assertSplitCommandPaths([spoof], w.ctx), refusal(SPLIT_TARGET_PATH_CODE));
  }
});

test("[N1/T7] the steps a release runs are all scanned (prePackage, package, postBuild, postSign, preSeal, publish, packageHook, recipe prepare and postBuild inside a recipe plan); the scraperight shape fails until its paths are absolute, then passes", (t) => {
  const w = world(t);
  const bad = "src-tauri/target/release/bundle/macos/Scrape.app";
  for (const key of ["prePackage", "package", "postBuild", "postSign", "preSeal", "publish"]) {
    const steps = releaseCommandSteps({ target: { [key]: [{ cmd: "node", args: ["a.mjs"] }, { cmd: "node", args: ["b.mjs", bad] }] }, config: {}, root: w.snapshot });
    assert.throws(() => assertSplitCommandPaths(steps, w.ctx), refusal(SPLIT_TARGET_PATH_CODE, new RegExp(`${key}\\[1\\]`)), key);
  }
  assert.throws(() => assertSplitCommandPaths(releaseCommandSteps({ target: {}, config: { nativeAssembly: { packageHook: { cmd: "node", args: ["h.mjs", bad] } } }, root: w.snapshot }), w.ctx), refusal(SPLIT_TARGET_PATH_CODE, /packageHook/));
  const plan = { version: 1, steps: [{ phase: "prepare", cmd: "node", args: ["scripts/prepare-runtime.mjs", "post", "--app", bad] }, { phase: "build", cmd: "pnpm", args: ["exec", "tauri", "build"] }] };
  const wrapped = { cmd: process.execPath, args: [path.join(HERE, "recipe-build.mjs"), "--plan", JSON.stringify(plan)] };
  assert.throws(() => assertSplitCommandPaths(releaseCommandSteps({ target: { prePackage: wrapped }, config: {}, root: w.snapshot }), w.ctx), refusal(SPLIT_TARGET_PATH_CODE, /recipe step 1/), "a step inside the recipe plan");
  assert.throws(() => assertSplitCommandPaths(releaseCommandSteps({ target: { macPackage: { prepare: [{ cmd: "node", args: ["p.mjs", bad] }] } }, config: {}, root: w.snapshot }), w.ctx), refusal(SPLIT_TARGET_PATH_CODE, /macPackage\.prepare\[0\]/), "an unexpanded recipe");
  // T7: scraperight's two refused steps, then the same steps with the absolute form resolveTargetRoot gives.
  const scrape = (macApp) => ({ postBuild: { cmd: "node", args: ["scripts/prepare-runtime.mjs", "post", "--app", macApp] }, postSign: { cmd: "node", args: ["scripts/verify-macos-release-artifacts.mjs", `${macApp}.tar.gz`] } });
  assert.throws(() => assertSplitCommandPaths(releaseCommandSteps({ target: scrape(bad), config: {}, root: w.snapshot }), w.ctx), refusal(SPLIT_TARGET_PATH_CODE, /postBuild .*postSign|postSign .*postBuild/s));
  const migrated = path.join(w.bound, "release/bundle/macos/Scrape.app");
  const steps = releaseCommandSteps({ target: scrape(migrated), config: {}, root: w.snapshot });
  assert.doesNotThrow(() => assertSplitCommandPaths(steps, w.ctx));
  assert.deepEqual(steps.map((step) => step.args.at(-1)), [migrated, `${migrated}.tar.gz`], "the step receives the absolute paths");
  assert.ok(steps.every((step) => path.isAbsolute(step.args.at(-1))));
});

// ---- A. the recipe mapper (R-T2, R-T3, R-T5) ----------------------------------------------------------------------------------------

const planOf = (command) => JSON.parse(command.args[2]);
const macTarget = (extra = {}) => ({ signed: true, macPackage: { cargoTargetRoot: true }, notarize: { app: "src-tauri/target/universal-apple-darwin/release/bundle/macos/Demo.app", file: "dist/Demo_universal.dmg", profile: "p" }, ...extra });
const winTarget = (extra = {}) => ({
  signed: true, winPackage: { installer: "nsis", productName: "Demo", updater: true, cargoTargetRoot: true },
  sign: { prePackageFiles: ["src-tauri/target/x86_64-pc-windows-msvc/release/demo.exe"], files: ["src-tauri/target/x86_64-pc-windows-msvc/release/bundle/nsis/Demo_1.0.0_x64-setup.exe"] }, ...extra,
});

test("[N7] a recipe that stages #924 copies in checkout mode plans ZERO copies in a split: bound absolute paths, in-place signing, no staged entry, no recipe-build stage", (t) => {
  const root = tmp(t); const appRoot = path.join(root, "snapshot"); const managedRoot = path.join(root, ".rightkit-managed"); const targetDir = path.join(managedRoot, "ws", "g", "1", "target");
  put(appRoot, "src-tauri/Cargo.toml", MANIFEST);
  const common = { appRoot, config: {}, resolveTargetRoot: () => targetDir, managedRoots: [managedRoot] };
  // The checkout route (N10): the broker target outside the app is staged by copy, declared paths stay app-relative.
  const checkoutWin = expandWinPackageTarget(winTarget(), { ...common, env: {} });
  assert.ok(planOf(checkoutWin.prePackage).stage.post.length >= 1, "control: checkout mode plans the #924 copy");
  assert.deepEqual(checkoutWin.sign.prePackageFiles, ["src-tauri/target/x86_64-pc-windows-msvc/release/demo.exe"]);
  const checkoutMac = expandMacPackageTarget(macTarget(), { ...common, env: {} });
  assert.ok(planOf(checkoutMac.prePackage).stage.post.length >= 1);
  // The split: the same inputs.
  const env = { RK_GATE_CHECKOUT: path.join(root, "checkout") };
  const win = expandWinPackageTarget(winTarget(), { ...common, env });
  const raw = path.join(targetDir, "x86_64-pc-windows-msvc", "release", "demo.exe");
  const installer = path.join(targetDir, "x86_64-pc-windows-msvc", "release", "bundle", "nsis", "Demo_1.0.0_x64-setup.exe");
  assert.deepEqual(win.sign.prePackageFiles, [raw]); assert.deepEqual(win.sign.files, [installer]);
  const pre = planOf(win.prePackage);
  assert.deepEqual(pre.boundTargets, [targetDir]); assert.equal(pre.stage, undefined, "no copy after the build");
  assert.deepEqual(pre.stability, { files: [raw] }); assert.deepEqual(pre.cleanup, { remove: [raw, installer] }, "the stale raw EXE is still removed, at the resolved path");
  assert.equal(win.package.cmd, "pnpm", "tauri bundle runs directly: it embeds the in-place signed EXE, with no copy back");
  assert.doesNotMatch(JSON.stringify(win), /"to":"src-tauri|stage/);
  const mac = expandMacPackageTarget(macTarget({ macPackage: { cargoTargetRoot: true, cleanup: { staleApps: true }, updater: true } }), { ...common, env });
  const app = path.join(targetDir, "universal-apple-darwin", "release", "bundle", "macos", "Demo.app");
  assert.equal(mac.notarize.app, app); assert.deepEqual(mac.sign.prePackageFiles, [app]);
  const macPlan = planOf(mac.prePackage);
  assert.equal(macPlan.stage, undefined); assert.deepEqual(macPlan.boundTargets, [targetDir]); assert.equal(macPlan.cleanup.apps.dir.replaceAll("\\", "/"), path.dirname(app).replaceAll("\\", "/"));
  assert.deepEqual(mac.postSign[0].args.slice(1), ["--app", app, "--output", "Demo.app.tar.gz"], "the updater tarball is a root, ignored output: written under the snapshot app root");
  // A target inside the snapshot is refused (never bound there); an unresolvable one is UNRESOLVED.
  assert.throws(() => expandMacPackageTarget(macTarget(), { ...common, env, resolveTargetRoot: () => path.join(appRoot, "src-tauri", "target") }), refusal(SPLIT_TARGET_UNRESOLVED_CODE, /inside the snapshot/));
  assert.throws(() => expandWinPackageTarget(winTarget(), { ...common, env, resolveTargetRoot: () => { throw new Error("no cargo"); } }), refusal(SPLIT_TARGET_UNRESOLVED_CODE, /never falls back/));
  // The mapper never fails "outside the app" for a target outside the app in a split (the L case, P3).
  assert.doesNotThrow(() => expandWinPackageTarget(winTarget(), { ...common, env, managedRoots: [] }));
});

// ---- A. the preflight (R-T1 in place of the target bridge, R-T5) ----------------------------------------------------------------------

test("[N5/R-T5] the preflight in a split replaces the target-bridge check with the R-T1 check; a target/ planted in the snapshot is refused with its fix", (t) => {
  const w = world(t);
  const checks = collectPreflight({ platform: "mac", appRoot: w.snapshot, repoRoot: w.checkout, nativeLayout: w.nativeLayout, splitTargets: w.ctx, rust: true, bridge: true, findSdkTool: () => null, env: { PATH: "" } });
  const names = checks.map((check) => check.name);
  assert.ok(names.includes("split-target")); assert.ok(!names.includes("target-bridge"), "the bridge check is a checkout-route rule");
  assert.equal(checks.find((check) => check.name === "split-target").status, "ok");
  assert.match(checks.find((check) => check.name === "split-target").detail, /bound-target/);
  put(w.snapshot, "src-tauri/target/debug/planted");
  const planted = collectPreflight({ platform: "mac", appRoot: w.snapshot, repoRoot: w.checkout, nativeLayout: w.nativeLayout, splitTargets: w.ctx, rust: true, bridge: true, findSdkTool: () => null, env: { PATH: "" } }).find((check) => check.name === "split-target");
  assert.notEqual(planted.status, "ok"); assert.match(planted.detail, /RK_SPLIT_TARGET_PATH: .*src-tauri[\\/]target[\\/]? exists in the snapshot/);
  const checkoutRoute = collectPreflight({ platform: "mac", appRoot: w.snapshot, repoRoot: w.checkout, nativeLayout: w.nativeLayout, rust: true, bridge: true, findSdkTool: () => null, env: { PATH: "" } }).map((check) => check.name);
  assert.ok(checkoutRoute.includes("target-bridge") && !checkoutRoute.includes("split-target"), "N10: the checkout route keeps its bridge check exactly");
});

// ---- B. sealing from the bound target (T2 fixture, T5, N2) ----------------------------------------------------------------------------------

const buildSource = fs.readFileSync(path.join(HERE, "build-release.mjs"), "utf8").replace(/\r\n/g, "\n");
function sealer(w, copies) {
  const start = buildSource.indexOf("function sealRelease(");
  const end = buildSource.indexOf("\nfunction collectToolVersions", start);
  return runInNewContext(`${buildSource.slice(start, end)}\nsealRelease;`, {
    ...fs, path, process, ArtifactPathError, resolveSplitArtifactPath: resolveArtifactPath, boundTargetsEvidence,
    assertUpdaterSignature: () => {}, isSuiteUpdaterApp: () => false, resolveUpdaterTrust: () => ({}), assertUpdaterKeyId: () => {},
    hashFile: (file) => require_sha(fs.readFileSync(file)), copyFileSync: (from, to) => { copies.push([from, to]); fs.copyFileSync(from, to); },
    writeJson: (file, value) => fs.writeFileSync(file, JSON.stringify(value)), fail: (message) => { throw new Error(message); },
    resolveArtifactDir: () => w.artifactDir, materializeArtifacts: () => {}, completeArtifactLease: () => {}, retainArtifacts: () => ({ removed: [] }),
    resolveDevtoolsAllowance: () => ({ mode: "denied" }), devtoolsEvidence: (value) => value,
  });
}
const require_sha = (bytes) => createHash("sha256").update(bytes).digest("hex");

function sealOptions(w, over = {}) {
  return { configRoot: w.snapshot, managedCargoTarget: null, artifactPaths: w.ctx, nativeLayout: w.nativeLayout, sealedDir: path.join(w.root, "sealed"), releaseId: "app-1.0.0-aaaaaaaa", platform: "win", commit: "a".repeat(40),
    config: { app: "app", version: "1.0.0", distribution: {} }, target: { artifacts: ["src-tauri/target/release/app.bin"] }, inputHashes: {}, toolVersions: {}, cacheKey: "k", ...over };
}

test("[T2 fixture, T5] sealing in a split reads the declared artifact IN PLACE in the bound target: the seal's bytes are the bound target's, the manifest records the bound target, and nothing was written under the snapshot", (t) => {
  const w = world(t);
  put(w.bound, "release/app.bin", "THE BOUND TARGET'S BYTES");
  const before = fs.readdirSync(w.snapshot);
  const copies = []; const seal = sealer(w, copies);
  seal(sealOptions(w));
  assert.deepEqual(copies.map(([from]) => from), [path.join(w.bound, "release", "app.bin")], "the one copy is out of the bound target");
  const manifest = JSON.parse(fs.readFileSync(path.join(w.root, "sealed", "release-manifest.json"), "utf8"));
  assert.deepEqual(manifest.boundTargets, [{ prefix: "src-tauri/target/", manifest: "src-tauri/Cargo.toml", dir: w.bound }]);
  assert.equal(manifest.files[0].sha256, require_sha("THE BOUND TARGET'S BYTES"));
  assert.deepEqual(fs.readdirSync(w.snapshot), before, "no target directory, link or copy under the snapshot");
  assert.ok(!fs.existsSync(path.join(w.snapshot, "src-tauri", "target")));
  // T5: an absolute declared path from resolveTargetRoot (postright, cutright) seals the same bytes.
  const absolute = path.join(w.bound, "release", "app.bin");
  const second = []; sealer(w, second)(sealOptions(w, { sealedDir: path.join(w.root, "sealed2"), target: { artifacts: [absolute] } }));
  assert.deepEqual(second.map(([from]) => from), [absolute]);
});

test("[N2/N4 at the seal] the seal refuses an absolute path into the checkout, and never seals a stale copy the checkout holds: the missing artifact is named", (t) => {
  const w = world(t);
  put(w.checkout, "src-tauri/target/release/app.bin", "STALE IN THE CHECKOUT"); put(w.checkout, "App.dmg", "STALE");
  const copies = []; const seal = sealer(w, copies);
  assert.throws(() => seal(sealOptions(w, { target: { artifacts: [path.join(w.checkout, "src-tauri/target/release/app.bin")] } })), /RK_SPLIT_TARGET_PATH/);
  assert.throws(() => seal(sealOptions(w, { target: { artifacts: ["App.dmg"] } })), /cannot seal missing artifact: .*snapshot[\\/]App\.dmg/);
  assert.throws(() => seal(sealOptions(w)), /cannot seal missing artifact: .*bound-target[\\/]release[\\/]app\.bin/, "the bound target holds none and the checkout's is not a fallback");
  assert.equal(copies.length, 0);
});

// ---- C. the RightKit subcommands as split children -------------------------------------------------------------------------------------

function childEnv(w, extra = {}) {
  const env = { ...process.env, RK_GATE_CHECKOUT: w.checkout, RIGHT_RELEASE_ARTIFACT_DIR: w.artifactDir, CARGO_TARGET_DIR: w.bound, ...extra };
  delete env.RK_GATE_RUN;
  return env;
}

test("[T6] create-mac-updater in a split reads the bound target and writes the updater under the snapshot app root; a target-prefixed --app and an absolute one in the bound target agree; a checkout path is refused", (t) => {
  const w = world(t);
  put(w.bound, "release/bundle/macos/Demo.app/Contents/Info.plist", "x");
  const script = path.join(HERE, "create-mac-updater.mjs");
  const run = (app, output = "Demo.app.tar.gz") => spawnSync(process.execPath, [script, "--dry-run", "--app", app, "--output", output], { cwd: w.snapshot, env: childEnv(w), encoding: "utf8" });
  const prefixed = run("src-tauri/target/release/bundle/macos/Demo.app");
  assert.equal(prefixed.status, 0, `${prefixed.stdout}\n${prefixed.stderr}`);
  assert.match(prefixed.stdout, new RegExp(`-czf ${w.snapshot.replaceAll("\\", "\\\\")}[\\\\/]Demo\\.app\\.tar\\.gz -C ${path.join(w.bound, "release", "bundle", "macos").replaceAll("\\", "\\\\")} `), "the tarball in the snapshot app root, the app from the bound target");
  const absolute = run(path.join(w.bound, "release/bundle/macos/Demo.app"));
  assert.equal(absolute.stdout, prefixed.stdout);
  assert.equal(fs.existsSync(path.join(w.snapshot, "src-tauri", "target")), false, "no target created in the snapshot");
  const stale = run(path.join(w.checkout, "src-tauri/target/release/bundle/macos/Demo.app"));
  assert.notEqual(stale.status, 0); assert.match(stale.stderr, /RK_SPLIT_TARGET_PATH/);
  // N10: outside a split the same call resolves against the cwd exactly as it always did.
  const env = childEnv(w); delete env.RK_GATE_CHECKOUT;
  const plain = spawnSync(process.execPath, [script, "--dry-run", "--app", "src-tauri/target/release/bundle/macos/Demo.app", "--output", "Demo.app.tar.gz"], { cwd: w.snapshot, env, encoding: "utf8" });
  assert.match(plain.stdout, new RegExp(`-C ${path.join(w.snapshot, "src-tauri", "target", "release", "bundle", "macos").replaceAll("\\", "\\\\")} `));
});

test("[T6] sign-updater in a split signs the file in the bound target (a prefixed or absolute --file), and refuses the checkout's", (t) => {
  const w = world(t);
  const logs = [];
  const ctx = w.ctx;
  const run = (file) => runSignUpdater(["--file", file, "--dry-run"], { cwd: w.snapshot, env: {}, artifactPaths: ctx, log: (line) => logs.push(line) });
  assert.deepEqual(run("src-tauri/target/release/bundle/macos/Demo.app.tar.gz").files, [path.join(w.bound, "release", "bundle", "macos", "Demo.app.tar.gz")]);
  assert.deepEqual(run(path.join(w.bound, "x.tar.gz")).files, [path.join(w.bound, "x.tar.gz")]);
  assert.deepEqual(run("Demo.app.tar.gz").files, [path.join(w.snapshot, "Demo.app.tar.gz")], "a root updater is the snapshot's");
  assert.throws(() => run(path.join(w.checkout, "Demo.app.tar.gz")), refusal(SPLIT_TARGET_PATH_CODE));
  // N10: without the split context it is path.resolve(cwd, file), as always.
  assert.deepEqual(runSignUpdater(["--file", "src-tauri/target/x", "--dry-run"], { cwd: w.snapshot, env: {}, log: () => {} }).files, [path.join(w.snapshot, "src-tauri", "target", "x")]);
});

test("[T6] mirror-root-artifact in a split mirrors a file from the bound target into the managed directory and refuses the checkout's; the snapshot has no git directory", (t) => {
  const w = world(t);
  put(w.bound, "release/Demo_setup.exe", "INSTALLER");
  const script = path.join(HERE, "mirror-root-artifact.mjs");
  const run = (file) => spawnSync(process.execPath, [script, "--file", file, "--package-root", w.snapshot], { cwd: w.snapshot, env: childEnv(w), encoding: "utf8" });
  const ok = run("src-tauri/target/release/Demo_setup.exe");
  assert.equal(ok.status, 0, `${ok.stdout}\n${ok.stderr}`);
  assert.equal(fs.readFileSync(path.join(w.artifactDir, "Demo_setup.exe"), "utf8"), "INSTALLER");
  const bad = run(path.join(w.checkout, "Demo_setup.exe"));
  assert.equal(bad.status, 2); assert.match(bad.stderr, /RK_SPLIT_TARGET_PATH/);
});

// ---- D. the real build-release.mjs as a split child -------------------------------------------------------------------------------------

/** A Tauri-shaped fixture app, a snapshot of its commit and a LIVE door run naming them; `targetExtra` shapes the mac target. */
function splitApp(t, { targetExtra = {}, files = {} } = {}) {
  const config = fixture({ platform: "mac", macSigning: true, packageCommand: { cmd: "false" }, targetExtra, files });
  const repo = fs.realpathSync(path.dirname(config));
  const c1 = fixtureGit(repo, "rev-parse", "HEAD"); const tree = fixtureGit(repo, "rev-parse", `${c1}^{tree}`);
  const snap = path.join(tmp(t, "rk-art-snap-"), "snapshot"); fs.mkdirSync(snap);
  const archive = path.join(path.dirname(snap), "c1.tar");
  assert.equal(spawnSync("git", ["-C", repo, "archive", "-o", archive, c1]).status, 0);
  assert.equal(spawnSync("tar", ["-xf", archive, "-C", snap]).status, 0);
  const nonce = "cd".repeat(16);
  const runsDir = path.join(repo, ".git", "rightkit-gate", "runs"); fs.mkdirSync(runsDir, { recursive: true });
  fs.writeFileSync(path.join(runsDir, `${nonce}.json`), JSON.stringify({ pid: process.pid, commit: c1, tree, stage: "package", source: { checkout: repo, snapshot: fs.realpathSync(snap), commit: c1, tree, appDir: "." }, run: { pid: process.pid, commit: c1, tree, stage: "package", source: { checkout: repo, snapshot: fs.realpathSync(snap), commit: c1, tree, appDir: "." } } }));
  const toolLog = path.join(tmp(t, "rk-art-log-"), "tools.log");
  const bound = path.join(tmp(t, "rk-art-bound-"), "target");
  const env = { ...managedEnv(config, { platform: "mac" }), RK_GATE_RUN: nonce, RK_GATE_CHECKOUT: repo, CARGO_TARGET_DIR: bound, RIGHT_RELEASE_TEST_TOOL_LOG: toolLog };
  const run = (e = env) => spawnSync(process.execPath, [path.join(HERE, "build-release.mjs"), "--platform", "mac", "--config", path.join(fs.realpathSync(snap), path.basename(config))], { cwd: fs.realpathSync(snap), env: e, encoding: "utf8" });
  const tools = () => (fs.existsSync(toolLog) ? fs.readFileSync(toolLog, "utf8").split("\n").filter(Boolean) : []);
  return { config, repo, snap: fs.realpathSync(snap), bound, env, run, tools, toolLog };
}

test("[N1 real] a split build whose config has a non-RightKit step naming src-tauri/target/... is refused RK_SPLIT_TARGET_PATH BEFORE anything builds: no pnpm install, no cargo build, no tauri, no target in the snapshot", (t) => {
  const fx = splitApp(t, {
    targetExtra: { postSign: [{ cmd: "node", args: ["scripts/verify-macos-release-artifacts.mjs", "src-tauri/target/release/bundle/macos/Fixture.app.tar.gz"] }] },
    files: { "scripts/verify-macos-release-artifacts.mjs": "process.exit(0);\n" },
  });
  const r = fx.run();
  assert.notEqual(r.status, 0, `${r.stdout}\n${r.stderr}`);
  assert.match(r.stderr, /RK_SPLIT_TARGET_PATH: postSign \(node\) names the target-relative path src-tauri\/target\/release\/bundle\/macos\/Fixture\.app\.tar\.gz/);
  assert.match(r.stderr, /resolveTargetRoot/, "the message names the fix");
  const launched = fx.tools().filter((line) => !/^(?:(?:pnpm|cargo|rustc|sccache|xcrun|rightkit-credentials)(?: --version| -vV| metadata)?|security find-(?:identity|generic-password).*)$/.test(line));
  assert.deepEqual(launched, [], `no build tool ran: ${fx.tools().join(" | ")}`);
  assert.ok(!fx.tools().some((line) => /^(pnpm (install|run|exec)|cargo build|tauri)/.test(line)), "no pnpm install, cargo build or tauri");
  assert.equal(fs.existsSync(path.join(fx.snap, "src-tauri", "target")), false);
  // The same config with the migrated (absolute) path is not refused by this rule.
  const migrated = splitApp(t, {
    targetExtra: { postSign: [{ cmd: "node", args: ["scripts/verify-macos-release-artifacts.mjs", "/abs/bound/release/bundle/macos/Fixture.app.tar.gz"] }] },
    files: { "scripts/verify-macos-release-artifacts.mjs": "process.exit(0);\n" },
  });
  assert.doesNotMatch(migrated.run().stderr, /RK_SPLIT_TARGET_PATH/);
});

test("[N8 real] a split whose Cargo metadata answers a target inside the snapshot (an unpinned split) is refused RK_SPLIT_TARGET_UNRESOLVED: nothing built, no snapshot-relative fallback", (t) => {
  const fx = splitApp(t);
  const env = { ...fx.env }; delete env.CARGO_TARGET_DIR;
  const r = fx.run(env);
  assert.notEqual(r.status, 0, `${r.stdout}\n${r.stderr}`);
  assert.match(r.stderr, /RK_SPLIT_TARGET_UNRESOLVED: .*resolves inside the snapshot/);
  assert.ok(!fx.tools().some((line) => /^(pnpm (install|run|exec)|cargo build|tauri)/.test(line)));
  assert.equal(fs.existsSync(path.join(fx.snap, "src-tauri", "target")), false);
});

test("[N7 real] a split build refused before it builds creates no link or junction anywhere (fs recorder on symlinkSync, symlink, junction)", (t) => {
  const fx = splitApp(t, { targetExtra: { postSign: [{ cmd: "node", args: ["x.mjs", "src-tauri/target/y"] }] } });
  const log = path.join(tmp(t, "rk-art-fs-"), "links.log");
  const recorder = path.join(path.dirname(log), "record-links.mjs");
  fs.writeFileSync(recorder, `import fs from "node:fs";\nconst log = process.env.RK_LINK_LOG;\nfor (const name of ["symlinkSync", "symlink", "linkSync", "link"]) { const real = fs[name]; fs[name] = (...a) => { fs.appendFileSync(log, JSON.stringify({ name, a: a.slice(0, 2).map(String) }) + "\\n"); return real(...a); }; }\n`);
  const r = fx.run({ ...fx.env, RK_LINK_LOG: log, NODE_OPTIONS: `${process.env.NODE_OPTIONS ?? ""} --import=${new URL(`file://${recorder.replaceAll("\\", "/")}`).href}`.trim() });
  assert.notEqual(r.status, 0);
  const links = fs.existsSync(log) ? fs.readFileSync(log, "utf8").split("\n").filter(Boolean).map((line) => JSON.parse(line)) : [];
  assert.deepEqual(links.filter((entry) => entry.a.some((arg) => arg.startsWith(fx.snap))), [], "no link into or inside the snapshot");
});

// ---- E. wiring present in every file the decision names (source form) ----------------------------------------------------------------------------

test("[7.04c] every caller the decision lists takes the one resolver (source form): release.mjs, sealRelease, the recipe mapper and plan validation, win-package-recipe, create-mac-updater, sign-updater, mirror-root-artifact, preflight", () => {
  const read = (file) => fs.readFileSync(path.join(HERE, file), "utf8").replace(/\r\n/g, "\n");
  assert.match(read("release.mjs"), /return resolveArtifactPath\(declared, artifactContext\(\)\)/);
  assert.match(read("release.mjs"), /assertSplitCommandPaths\(releaseCommandSteps\(\{ target, config, root \}\), artifactContext\(\)\)/);
  for (const direct of ["path.resolve(root, target.msix.stageDir)", "target.sign.prePackageFiles.map((p) => path.resolve(root, p))", "const rawFile = path.resolve(root, target.sign.prePackageFiles[0])"]) assert.ok(!read("release.mjs").includes(direct), `release.mjs resolves ${direct} through the resolver`);
  assert.match(read("build-release.mjs"), /resolveSplitArtifactPath\(file, artifactPaths\)/);
  assert.match(read("package-recipe-targets.mjs"), /isSplitArtifactEnv\(env\)\) return splitTargetRootMapper/);
  assert.match(read("recipe-build.mjs"), /boundTargets/);
  assert.match(read("win-package-recipe.mjs"), /resolveArtifactPath\(relative\.startsWith/);
  for (const file of ["create-mac-updater.mjs", "sign-updater.mjs", "mirror-root-artifact.mjs"]) assert.match(read(file), /release-artifact-path\.mjs/, file);
  assert.match(read("preflight.mjs"), /splitTargets\?\.mode === "split"/);
});
