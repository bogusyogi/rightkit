import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, renameSync as renameSyncReal, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { runRecipeBuild } from "./recipe-build.mjs";
import { contentDigest, isUnderManagedRoot, ManagedTargetStageError, stageArtifact } from "./managed-target-stage.mjs";

const sha = (value) => createHash("sha256").update(value).digest("hex");
function workspace() {
  const base = mkdtempSync(path.join(tmpdir(), "rk-stage-"));
  const app = path.join(base, "app");
  const managed = path.join(base, ".rightkit-managed", "rightkit-build-control", "ws", "h", "g", "1", "t");
  mkdirSync(app, { recursive: true });
  mkdirSync(managed, { recursive: true });
  return { base, app, managed, done: () => rmSync(base, { recursive: true, force: true }) };
}

test("stageArtifact copies atomically, verifies sha256 and leaves no temp file", (t) => {
  const w = workspace(); t.after(w.done);
  const from = path.join(w.managed, "demo.exe");
  const to = path.join(w.app, "src-tauri", "target", "release", "demo.exe");
  writeFileSync(from, "fresh bytes");
  const result = stageArtifact({ from, to });
  assert.equal(result.sha256, sha("fresh bytes"));
  assert.equal(readFileSync(to, "utf8"), "fresh bytes");
  assert.deepEqual(readdirSync(path.dirname(to)), ["demo.exe"]);
});

test("a missing source fails with a clear message and the stale destination is gone, never packaged", (t) => {
  const w = workspace(); t.after(w.done);
  const to = path.join(w.app, "demo.exe");
  writeFileSync(to, "STALE");
  assert.throws(() => stageArtifact({ from: path.join(w.managed, "demo.exe"), to }), (error) => error instanceof ManagedTargetStageError && error.code === "STAGE_SOURCE_MISSING" && /did not produce .*demo\.exe.*no stale artifact/.test(error.message));
  assert.equal(existsSync(to), false);
});

test("a copy that does not match its source is refused and nothing is renamed into place", (t) => {
  const w = workspace(); t.after(w.done);
  const from = path.join(w.managed, "demo.exe");
  const to = path.join(w.app, "demo.exe");
  writeFileSync(from, "good");
  const corrupt = { cpSync: (src, dest) => writeFileSync(dest, "bad") };
  assert.throws(() => stageArtifact({ from, to }, { fs: corrupt }), (error) => error.code === "STAGE_DIGEST_MISMATCH");
  assert.deepEqual(readdirSync(w.app), []);
});

test("a directory (.app) is copied with symlinks preserved and compared as a tree", (t) => {
  const w = workspace(); t.after(w.done);
  const from = path.join(w.managed, "Demo.app");
  mkdirSync(path.join(from, "Contents", "MacOS"), { recursive: true });
  writeFileSync(path.join(from, "Contents", "MacOS", "demo"), "bin", { mode: 0o755 });
  symlinkSync("MacOS/demo", path.join(from, "Contents", "link"));
  const to = path.join(w.app, "bundle", "Demo.app");
  const result = stageArtifact({ from, to });
  assert.equal(result.kind, "directory");
  assert.equal(contentDigest(to).sha256, contentDigest(from).sha256);
});

test("isUnderManagedRoot: strictly inside a managed root only", (t) => {
  const w = workspace(); t.after(w.done);
  const root = path.join(w.base, ".rightkit-managed");
  assert.equal(isUnderManagedRoot(w.managed, [root]), true);
  assert.equal(isUnderManagedRoot(root, [root]), false);
  assert.equal(isUnderManagedRoot(path.join(w.base, "elsewhere"), [root]), false);
  assert.equal(isUnderManagedRoot(path.join(w.base, ".rightkit-managed-other", "x"), [root]), false);
  assert.equal(isUnderManagedRoot(w.managed, []), false);
  assert.equal(isUnderManagedRoot("D:\\.RightKit-Managed\\x", ["D:\\.rightkit-managed"], { platform: "win32" }), path.sep === "\\");
});

test("recipe-build runs stage.pre before and stage.post after the build step, removes fresh sources first, then polls stability", (t) => {
  const w = workspace(); t.after(w.done);
  const rawTarget = path.join(w.managed, "demo.exe");
  const installerTarget = path.join(w.managed, "setup.exe");
  writeFileSync(path.join(w.app, "demo.exe"), "SIGNED");
  writeFileSync(installerTarget, "STALE INSTALLER");
  const events = [];
  const spawnSync = () => {
    events.push({ rawBeforeBuild: readFileSync(rawTarget, "utf8"), staleInstallerGone: !existsSync(installerTarget) });
    writeFileSync(installerTarget, "NEW INSTALLER");
    return { status: 0 };
  };
  const result = runRecipeBuild({
    version: 1,
    steps: [{ phase: "build", cmd: "pnpm", args: ["bundle"] }],
    stage: { pre: [{ from: "demo.exe", to: rawTarget }], post: [{ from: installerTarget, to: "setup.exe", fresh: true }] },
    stability: { files: ["setup.exe"], intervalMs: 1, checks: 1 },
  }, { appRoot: w.app, spawnSync, sleep: () => {} });
  assert.deepEqual(events, [{ rawBeforeBuild: "SIGNED", staleInstallerGone: true }]);
  assert.equal(readFileSync(path.join(w.app, "setup.exe"), "utf8"), "NEW INSTALLER");
  assert.equal(result.staged.length, 2);
});

test("recipe-build: a build that leaves no fresh output fails the stage step", (t) => {
  const w = workspace(); t.after(w.done);
  const installerTarget = path.join(w.managed, "setup.exe");
  writeFileSync(installerTarget, "STALE");
  writeFileSync(path.join(w.app, "setup.exe"), "OLD COPY");
  assert.throws(() => runRecipeBuild({
    version: 1, steps: [{ phase: "build", cmd: "pnpm" }], stage: { post: [{ from: installerTarget, to: "setup.exe", fresh: true }] },
  }, { appRoot: w.app, spawnSync: () => ({ status: 0 }) }), { code: "STAGE_SOURCE_MISSING" });
  assert.equal(existsSync(path.join(w.app, "setup.exe")), false);
});

test("the stage plan is validated: app side relative and inside, other side absolute", () => {
  const bad = (stage) => assert.throws(() => runRecipeBuild({ version: 1, steps: [{ phase: "build", cmd: "x" }], stage }, { appRoot: path.resolve("/rk-app"), spawnSync: () => ({ status: 0 }) }), { code: "RECIPE_BUILD_PLAN_INVALID" });
  bad({ post: [{ from: "rel/out.exe", to: "out.exe" }] });
  bad({ post: [{ from: path.resolve("/t/out.exe"), to: "../out.exe" }] });
  bad({ pre: [{ from: "a.exe", to: "rel.exe" }] });
  bad({ pre: [{ from: "a.exe", to: path.resolve("/t/a.exe"), fresh: true }] });
  bad({ post: [{ from: path.resolve("/t/out.exe"), to: "out.exe", extra: 1 }] });
});

test("recipe-build: the real exe is deleted before the build step, and a build that does not rewrite it fails", (t) => {
  const w = workspace(); t.after(w.done);
  const real = path.join(w.managed, "demo.exe");
  writeFileSync(real, "PREVIOUS SIGNED");
  const plan = { version: 1, steps: [{ phase: "build", cmd: "pnpm" }], stage: { post: [{ from: real, to: "demo.exe", fresh: true }] } };
  const seen = [];
  runRecipeBuild(plan, { appRoot: w.app, sleep: () => {}, spawnSync: () => { seen.push(existsSync(real)); writeFileSync(real, "THIS BUILD"); return { status: 0 }; } });
  assert.deepEqual(seen, [false]);
  assert.equal(readFileSync(path.join(w.app, "demo.exe"), "utf8"), "THIS BUILD");
  writeFileSync(real, "PREVIOUS SIGNED");
  assert.throws(() => runRecipeBuild(plan, { appRoot: w.app, spawnSync: () => ({ status: 0 }) }), { code: "STAGE_SOURCE_MISSING" });
});

test("recipe-build: a still-changing real source fails the stability wait before any copy", (t) => {
  const w = workspace(); t.after(w.done);
  const real = path.join(w.managed, "demo.exe");
  writeFileSync(real, "x");
  writeFileSync(path.join(w.app, "demo.exe"), "OLD IN-APP COPY");
  let tick = 0;
  const statSync = () => ({ size: ++tick, mtimeMs: tick });
  let clock = 0;
  assert.throws(() => runRecipeBuild({
    version: 1, steps: [{ phase: "build", cmd: "pnpm" }],
    stage: { post: [{ from: real, to: "demo.exe" }] }, stability: { files: ["demo.exe"], intervalMs: 1, checks: 2, timeoutMs: 50 },
  }, { appRoot: w.app, spawnSync: () => ({ status: 0 }), statSync, sleep: () => { clock += 10; }, now: () => clock }), { code: "RECIPE_BUILD_ARTIFACT_UNSTABLE" });
  // Nothing was copied: the in-app copy is still what was there before.
  assert.equal(readFileSync(path.join(w.app, "demo.exe"), "utf8"), "OLD IN-APP COPY");
});

const bundlePlan = (w, extra = {}) => ({
  version: 1, steps: [{ phase: "build", cmd: "pnpm" }],
  stage: {
    pre: [{ from: "demo.exe", to: path.join(w.managed, "demo.exe") }],
    verify: [{ app: "demo.exe", real: path.join(w.managed, "demo.exe") }],
    resolve: { manifest: path.join(w.app, "Cargo.toml"), expected: w.managed },
    post: [{ from: path.join(w.managed, "setup.exe"), to: "setup.exe", fresh: true }],
    ...extra,
  },
});

test("C1: an exe that changes during bundle fails before the installer is copied out", (t) => {
  const w = workspace(); t.after(w.done);
  writeFileSync(path.join(w.app, "demo.exe"), "SIGNED");
  const spawnSync = () => { writeFileSync(path.join(w.managed, "setup.exe"), "INSTALLER"); writeFileSync(path.join(w.managed, "demo.exe"), "RELINKED BY ANOTHER JOB"); return { status: 0 }; };
  assert.throws(() => runRecipeBuild(bundlePlan(w), { appRoot: w.app, spawnSync, resolveTargetRoot: () => w.managed }), (error) => error.code === "STAGE_REAL_CHANGED" && /after the build step/.test(error.message));
  assert.equal(existsSync(path.join(w.app, "setup.exe")), false);
});

test("C1: an exe that differs from the signed copy right before bundle fails before the build step runs", (t) => {
  const w = workspace(); t.after(w.done);
  writeFileSync(path.join(w.app, "demo.exe"), "SIGNED");
  let ran = false;
  const fs = { cpSync: (src, dest) => writeFileSync(dest, readFileSync(src)) };
  const racing = { ...fs, renameSync: (a, b) => { writeFileSync(a, "RACED"); return renameSyncReal(a, b); } };
  assert.throws(() => runRecipeBuild(bundlePlan(w), { appRoot: w.app, fs: racing, spawnSync: () => { ran = true; return { status: 0 }; }, resolveTargetRoot: () => w.managed }), (error) => ["STAGE_REAL_CHANGED", "STAGE_DIGEST_MISMATCH"].includes(error.code));
  assert.equal(ran, false);
});

test("C1: the signed exe in the target directory and the in-app exe match on success", (t) => {
  const w = workspace(); t.after(w.done);
  writeFileSync(path.join(w.app, "demo.exe"), "SIGNED");
  const spawnSync = () => { writeFileSync(path.join(w.managed, "setup.exe"), "INSTALLER"); return { status: 0 }; };
  runRecipeBuild(bundlePlan(w), { appRoot: w.app, spawnSync, sleep: () => {}, resolveTargetRoot: () => w.managed });
  assert.equal(readFileSync(path.join(w.app, "setup.exe"), "utf8"), "INSTALLER");
});

test("C2: a rotated generation (re-resolved target directory differs) is refused before and after the build step", (t) => {
  const w = workspace(); t.after(w.done);
  writeFileSync(path.join(w.app, "demo.exe"), "SIGNED");
  let ran = false;
  assert.throws(() => runRecipeBuild(bundlePlan(w), { appRoot: w.app, spawnSync: () => { ran = true; return { status: 0 }; }, resolveTargetRoot: () => path.join(w.base, "g", "2", "t") }), (error) => error.code === "STAGE_TARGET_DIR_CHANGED" && /rotated or evicted/.test(error.message));
  assert.equal(ran, false);
  // Rotation during the step: the second resolution differs.
  let calls = 0;
  const spawnSync = () => { writeFileSync(path.join(w.managed, "setup.exe"), "I"); return { status: 0 }; };
  assert.throws(() => runRecipeBuild(bundlePlan(w), { appRoot: w.app, spawnSync, resolveTargetRoot: () => (++calls === 1 ? w.managed : path.join(w.base, "g", "2", "t")) }), { code: "STAGE_TARGET_DIR_CHANGED" });
  assert.equal(existsSync(path.join(w.app, "setup.exe")), false);
});

test("C2: a missing parent directory in the broker tree is never created", (t) => {
  const w = workspace(); t.after(w.done);
  writeFileSync(path.join(w.app, "demo.exe"), "SIGNED");
  const gone = path.join(w.base, "evicted", "t");
  assert.throws(() => stageArtifact({ from: path.join(w.app, "demo.exe"), to: path.join(gone, "demo.exe") }, { requireParent: true }), (error) => error.code === "STAGE_PARENT_MISSING" && /rotated or evicted/.test(error.message));
  assert.equal(existsSync(path.dirname(gone)), false);
  const plan = bundlePlan(w); plan.stage.pre = [{ from: "demo.exe", to: path.join(gone, "demo.exe") }]; plan.stage.verify = [];
  assert.throws(() => runRecipeBuild(plan, { appRoot: w.app, spawnSync: () => ({ status: 0 }), resolveTargetRoot: () => w.managed }), { code: "STAGE_PARENT_MISSING" });
  assert.equal(existsSync(path.dirname(gone)), false);
});
