import assert from "node:assert/strict";
import { existsSync, lstatSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, symlinkSync, utimesSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { QA_CONTROL_MARKER } from "./qa-control-scan.mjs";
import { QA_COMPILED_DIRS_ENV, compiledDirsFromEnv, findBuiltExecutables, stageQaLayout, validateQaLayout } from "./qa-layout.mjs";

const LAYOUT = { binary: "shell", beside: [
  { built: "engine" },
  { from: "src-tauri/resources", to: "resources" },
  { from: ["src-tauri/native-missing", "src-tauri/native-sources"], to: "resources/runtime" },
  { built: "extra", optional: true },
] };

// A snapshot app root, a compile output directory with a QA shell, and a runs root, all under one temp directory.
function withFixture(body, { shellBytes = `ELF ${QA_CONTROL_MARKER} shell`, profile = "debug" } = {}) {
  const dir = mkdtempSync(path.join(tmpdir(), "rk-qa-layout-"));
  try {
    const app = path.join(dir, "snapshot", "app");
    mkdirSync(path.join(app, "src-tauri", "resources", "ffmpeg"), { recursive: true });
    mkdirSync(path.join(app, "src-tauri", "native-sources"), { recursive: true });
    mkdirSync(path.join(app, "qa", "scenarios"), { recursive: true });
    writeFileSync(path.join(app, "src-tauri", "resources", "a.txt"), "resource-a");
    writeFileSync(path.join(app, "src-tauri", "resources", "ffmpeg", "ffmpeg"), "ffmpeg-bytes");
    writeFileSync(path.join(app, "src-tauri", "native-sources", "libort.dylib"), "ort");
    writeFileSync(path.join(app, "qa", "rightkit-qa.toml"), "app = 'demo'\napp_root = '..'\n");
    writeFileSync(path.join(app, "qa", "scenarios", "launch.toml"), "name = 'launch'\n");
    writeFileSync(path.join(app, "package.json"), "{}");
    const built = path.join(dir, "target");
    mkdirSync(path.join(built, profile), { recursive: true });
    writeFileSync(path.join(built, profile, "shell"), shellBytes);
    writeFileSync(path.join(built, profile, "engine"), "engine-bytes");
    const runsRoot = path.join(dir, "runs");
    return body({ dir, app, built, runsRoot, config: path.join(app, "qa", "rightkit-qa.toml") });
  } finally { rmSync(dir, { recursive: true, force: true }); }
}
const tree = (root) => {
  const out = [];
  const walk = (dir) => { for (const e of readdirSync(dir, { withFileTypes: true })) { const f = path.join(dir, e.name); out.push(path.relative(root, f)); if (e.isDirectory()) walk(f); } };
  walk(root);
  return out.sort();
};
const stage = (ctx, overrides = {}) => stageQaLayout({ layout: LAYOUT, snapshotAppRoot: ctx.app, qaConfigFile: ctx.config, runsRoot: ctx.runsRoot, dirs: [ctx.built], platform: "mac", ...overrides });

test("validateQaLayout accepts the documented shape and reports every problem at once", () => {
  assert.deepEqual(validateQaLayout(LAYOUT), []);
  assert.deepEqual(validateQaLayout({ binary: "shell" }), []);
  const problems = validateQaLayout({ binary: "a/b", extra: 1, beside: [
    { built: "x", from: "y", to: "z" }, { to: "q" }, { from: "../escape", to: "r" }, { from: "ok", to: "/abs" }, { from: "ok2" },
    { built: "dup" }, { built: "dup" }, { built: "e.exe" }, "str", { from: [], to: "t" }, { built: "o", optional: "yes" }, { built: "k", mystery: 1 },
  ] }).join("\n");
  for (const expected of [/binary must be a bare/, /unknown key "extra"/, /needs exactly one of built or from/, /from must be a relative path/, /\.to must be a relative path beside/, /\.to is required with from/, /stages dup a second time/, /\.built must be a bare/, /must be an object/, /optional must be a boolean/, /unknown key "mystery"/]) assert.match(problems, expected);
  assert.match(validateQaLayout(null).join(), /must be an object/);
  assert.match(validateQaLayout({ binary: "s", beside: "x" }).join(), /must be a list/);
});

test("compiledDirsFromEnv reads the gate's JSON list and refuses a malformed one", () => {
  assert.equal(compiledDirsFromEnv({}), null);
  assert.equal(compiledDirsFromEnv({ [QA_COMPILED_DIRS_ENV]: "" }), null);
  assert.deepEqual(compiledDirsFromEnv({ [QA_COMPILED_DIRS_ENV]: JSON.stringify(["/a", "/b"]) }), ["/a", "/b"]);
  for (const bad of ["not json", "[]", "{}", JSON.stringify(["relative"]), JSON.stringify([1])]) assert.throws(() => compiledDirsFromEnv({ [QA_COMPILED_DIRS_ENV]: bad }), /RK_QA_LAYOUT_DIRS/);
});

test("findBuiltExecutables reads dev profiles only (never release), with or without a triple, newest first", () => withFixture((ctx) => {
  mkdirSync(path.join(ctx.built, "release"), { recursive: true });
  writeFileSync(path.join(ctx.built, "release", "shell"), `${QA_CONTROL_MARKER}`);
  mkdirSync(path.join(ctx.built, "aarch64-apple-darwin", "release-iterate"), { recursive: true });
  const tripled = path.join(ctx.built, "aarch64-apple-darwin", "release-iterate", "shell");
  writeFileSync(tripled, `${QA_CONTROL_MARKER}`);
  utimesSync(path.join(ctx.built, "debug", "shell"), new Date(1000), new Date(1000));
  const found = findBuiltExecutables([ctx.built], "shell", "mac");
  assert.deepEqual(found.map((f) => [path.relative(ctx.built, f.file).replaceAll("\\", "/"), f.triple]), [["aarch64-apple-darwin/release-iterate/shell", "aarch64-apple-darwin"], ["debug/shell", "host"]]);
  assert.deepEqual(findBuiltExecutables([ctx.built], "shell", "win"), [], "a win build looks for shell.exe");
}));

test("stage: a run-scoped overlay holds the config copy, the shell in the stash and the declared layout; the snapshot is not written", () => withFixture((ctx) => {
  const before = tree(path.join(ctx.dir, "snapshot"));
  const staged = stage(ctx);
  try {
    assert.deepEqual(tree(path.join(ctx.dir, "snapshot")), before, "nothing was created under the snapshot");
    assert.ok(path.relative(ctx.runsRoot, staged.runDir).split(path.sep)[0].startsWith("qa-layout-"), "the run directory is under the runs root");
    assert.ok(!staged.runDir.startsWith(path.join(ctx.dir, "snapshot")));
    // the overlay: real directories only on the way to the config, which is a copy; every other entry is a read-only symlink into the snapshot
    assert.equal(staged.configFile, path.join(staged.appRoot, "qa", "rightkit-qa.toml"));
    assert.equal(lstatSync(staged.configFile).isSymbolicLink(), false);
    assert.equal(lstatSync(path.join(staged.appRoot, "qa")).isSymbolicLink(), false);
    assert.equal(readFileSync(staged.configFile, "utf8"), readFileSync(ctx.config, "utf8"));
    for (const name of ["package.json", "src-tauri", path.join("qa", "scenarios")]) assert.equal(lstatSync(path.join(staged.appRoot, name)).isSymbolicLink(), true, `${name} is a symlink`);
    assert.ok(!existsSync(path.join(staged.appRoot, ".git")));
    // the stash: the binary the runner is pointed at is already inside `<app root>/.rightkit-qa-bin`, so the runner never copies it elsewhere
    const besideDir = path.join(staged.appRoot, ".rightkit-qa-bin", "host");
    assert.equal(staged.binary, path.join(besideDir, "shell"));
    assert.equal(readFileSync(staged.binary, "utf8"), readFileSync(path.join(ctx.built, "debug", "shell"), "utf8"));
    assert.equal(readFileSync(path.join(besideDir, "engine"), "utf8"), "engine-bytes", "built executable beside the shell");
    assert.equal(readFileSync(path.join(besideDir, "resources", "a.txt"), "utf8"), "resource-a");
    assert.equal(readFileSync(path.join(besideDir, "resources", "ffmpeg", "ffmpeg"), "utf8"), "ffmpeg-bytes");
    assert.equal(readFileSync(path.join(besideDir, "resources", "runtime", "libort.dylib"), "utf8"), "ort", "the first existing fallback of a from list is used");
    assert.ok(!existsSync(path.join(besideDir, "extra")), "an optional built executable that is absent is skipped");
    // evidence: every staged file digested, the shell's origin named
    assert.equal(staged.evidence.source, path.join(ctx.built, "debug", "shell"));
    assert.match(staged.evidence.binary.sha256, /^[0-9a-f]{64}$/);
    assert.deepEqual(staged.evidence.beside.map((e) => e.path), ["engine", "resources/a.txt", "resources/ffmpeg/ffmpeg", "resources/runtime/libort.dylib"]);
    assert.ok(staged.evidence.beside.every((e) => /^[0-9a-f]{64}$/.test(e.sha256) && e.bytes > 0));
    assert.match(staged.evidence.qaConfig.sha256, /^[0-9a-f]{64}$/);
  } finally { staged.cleanup(); }
  assert.ok(!existsSync(staged.runDir), "cleanup removes the run directory");
}));

test("stage: a runner write into the overlay's own directories (the stash, the config directory) never reaches the snapshot", () => withFixture((ctx) => {
  const before = tree(path.join(ctx.dir, "snapshot"));
  const staged = stage(ctx);
  try {
    writeFileSync(path.join(staged.appRoot, "qa", "runner-wrote-this.json"), "{}");
    writeFileSync(path.join(staged.appRoot, ".rightkit-qa-bin", "host", "pointer"), "x");
    writeFileSync(path.join(staged.appRoot, ".rightkit-qa-app-binary"), "x");
    assert.deepEqual(tree(path.join(ctx.dir, "snapshot")), before);
  } finally { staged.cleanup(); }
}));

test("stage refuses: no binary, a plain app without the QA marker, an invalid layout, a runs root inside the snapshot, a missing source", () => {
  withFixture((ctx) => assert.throws(() => stage(ctx, { dirs: [path.join(ctx.dir, "nowhere")] }), /RK_QA_LAYOUT_NO_BINARY.*compile did not build the QA shell/), {});
  withFixture((ctx) => assert.throws(() => stage(ctx), /RK_QA_LAYOUT_NOT_QA.*plain app/), { shellBytes: "just an app" });
  withFixture((ctx) => assert.throws(() => stage(ctx), /RK_QA_LAYOUT_NO_BINARY/), { profile: "release" });
  withFixture((ctx) => {
    assert.throws(() => stage(ctx, { layout: { binary: "a/b" } }), /RK_QA_LAYOUT_INVALID/);
    assert.throws(() => stage(ctx, { runsRoot: path.join(ctx.app, "runs") }), /RK_QA_LAYOUT_PLACE/);
    assert.throws(() => stage(ctx, { runsRoot: "relative" }), /RK_QA_LAYOUT_PLACE/);
    assert.throws(() => stage(ctx, { layout: { binary: "shell", beside: [{ from: "src-tauri/gone", to: "x" }] } }), /RK_QA_LAYOUT_MISSING.*does not exist in the snapshot/);
    assert.throws(() => stage(ctx, { layout: { binary: "shell", beside: [{ built: "gone" }] } }), /RK_QA_LAYOUT_MISSING.*built executable gone/);
    const outside = path.join(ctx.dir, "outside.txt");
    writeFileSync(outside, "secret");
    symlinkSync(outside, path.join(ctx.app, "src-tauri", "escape"));
    assert.throws(() => stage(ctx, { layout: { binary: "shell", beside: [{ from: "src-tauri/escape", to: "x" }] } }), /RK_QA_LAYOUT_ESCAPE/);
    symlinkSync(outside, path.join(ctx.app, "src-tauri", "resources", "linked.txt"));
    assert.throws(() => stage(ctx), /RK_QA_LAYOUT_LINK/, "a link inside a copied tree is refused, not followed");
    assert.deepEqual(existsSync(ctx.runsRoot) ? readdirSync(ctx.runsRoot) : [], [], "a refused stage leaves no run directory behind");
    const stray = path.join(ctx.dir, "elsewhere", "rightkit-qa.toml");
    mkdirSync(path.dirname(stray), { recursive: true });
    writeFileSync(stray, "x");
    assert.throws(() => stage(ctx, { qaConfigFile: stray }), /RK_QA_LAYOUT_CONFIG/);
  });
});

test("qa.layout.build: a non-empty list of distinct suite names; anything else is reported", () => {
  assert.deepEqual(validateQaLayout({ ...LAYOUT, build: ["qa-build-shell", "qa-sign"] }), []);
  for (const build of [[], ["a", "a"], [""], "qa-build-shell", [1]]) assert.ok(validateQaLayout({ ...LAYOUT, build }).some((p) => p.includes("qa.layout.build")), JSON.stringify(build));
});

test("stage with a sign step's copies: the shell and every built entry are the signed bytes; one it did not sign is refused", () => withFixture((ctx) => {
  const signedDir = path.join(ctx.dir, "signed");
  mkdirSync(signedDir);
  writeFileSync(path.join(signedDir, "shell"), `ELF ${QA_CONTROL_MARKER} shell signed`);
  writeFileSync(path.join(signedDir, "engine"), "engine-bytes signed");
  const signed = { "debug/shell": path.join(signedDir, "shell"), "debug/engine": path.join(signedDir, "engine") };
  const staged = stage(ctx, { signed });
  try {
    assert.equal(readFileSync(staged.binary, "utf8"), `ELF ${QA_CONTROL_MARKER} shell signed`);
    assert.equal(readFileSync(path.join(path.dirname(staged.binary), "engine"), "utf8"), "engine-bytes signed");
    assert.equal(staged.evidence.signed, true);
  } finally { staged.cleanup(); }
  assert.throws(() => stage(ctx, { signed: { "debug/shell": signed["debug/shell"] } }), /RK_QA_LAYOUT_UNSIGNED.*debug\/engine/);
  assert.throws(() => stage(ctx, { signed: {} }), /RK_QA_LAYOUT_UNSIGNED.*debug\/shell/);
}));

test("a resource entry is staged into the app's resource directory: Resources/ beside the macOS shell (the bundle's Contents/Resources), the shell's own directory on Windows", () => withFixture((ctx) => {
  const layout = { binary: "shell", beside: [{ built: "engine" }, { from: "src-tauri/resources", to: ".", resource: true }] };
  assert.deepEqual(validateQaLayout(layout), []);
  assert.ok(validateQaLayout({ binary: "shell", beside: [{ built: "engine", resource: true }] }).some((p) => p.includes(".resource")));
  const mac = stageQaLayout({ layout, snapshotAppRoot: ctx.app, qaConfigFile: ctx.config, runsRoot: ctx.runsRoot, dirs: [ctx.built], platform: "mac" });
  try {
    const beside = path.dirname(mac.binary);
    assert.equal(readFileSync(path.join(beside, "Resources", "a.txt"), "utf8"), "resource-a");
    assert.equal(readFileSync(path.join(beside, "Resources", "ffmpeg", "ffmpeg"), "utf8"), "ffmpeg-bytes");
    assert.ok(existsSync(path.join(beside, "engine")));
  } finally { mac.cleanup(); }
  writeFileSync(path.join(ctx.built, "debug", "shell.exe"), readFileSync(path.join(ctx.built, "debug", "shell")));
  writeFileSync(path.join(ctx.built, "debug", "engine.exe"), "engine-bytes");
  const win = stageQaLayout({ layout, snapshotAppRoot: ctx.app, qaConfigFile: ctx.config, runsRoot: ctx.runsRoot, dirs: [ctx.built], platform: "win" });
  try {
    assert.equal(readFileSync(path.join(path.dirname(win.binary), "a.txt"), "utf8"), "resource-a");
  } finally { win.cleanup(); }
}));

test("platforms limits an entry to its hosts; two from entries may fill one directory, but not write one file with other bytes", () => withFixture((ctx) => {
  mkdirSync(path.join(ctx.app, "src-tauri", "native-sources", "more"), { recursive: true });
  writeFileSync(path.join(ctx.app, "src-tauri", "native-sources", "more", "libextra.dylib"), "extra");
  const layout = { binary: "shell", beside: [
    { from: "src-tauri/native-sources", to: "runtime", resource: true, platforms: ["mac"] },
    { from: "src-tauri/resources/ffmpeg", to: "runtime", resource: true },
    { from: "src-tauri/native-sources", to: "resources/runtime", resource: true, platforms: ["win"] },
  ] };
  assert.deepEqual(validateQaLayout(layout), []);
  assert.ok(validateQaLayout({ binary: "shell", beside: [{ from: "x", to: "y", platforms: ["linux"] }] }).some((p) => p.includes(".platforms")));
  const mac = stageQaLayout({ layout, snapshotAppRoot: ctx.app, qaConfigFile: ctx.config, runsRoot: ctx.runsRoot, dirs: [ctx.built], platform: "mac" });
  try {
    const res = path.join(path.dirname(mac.binary), "Resources");
    assert.equal(readFileSync(path.join(res, "runtime", "libort.dylib"), "utf8"), "ort");
    assert.equal(readFileSync(path.join(res, "runtime", "ffmpeg"), "utf8"), "ffmpeg-bytes", "a second entry fills the same directory");
    assert.ok(!existsSync(path.join(res, "resources")), "the win-only entry is not staged on mac");
  } finally { mac.cleanup(); }
  writeFileSync(path.join(ctx.app, "src-tauri", "resources", "ffmpeg", "libort.dylib"), "other");
  assert.throws(() => stageQaLayout({ layout, snapshotAppRoot: ctx.app, qaConfigFile: ctx.config, runsRoot: ctx.runsRoot, dirs: [ctx.built], platform: "mac" }), /RK_QA_LAYOUT_COLLISION/);
}));
