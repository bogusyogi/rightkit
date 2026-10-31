// `rightkit-gate --to tool --unit cargo:rightkit-qa` (tool-build.mjs): planning, refusals and the build-then-enrol sequence. No real cargo runs:
// the cargo call, the class A check and the runner's `--version` are doubles; the enrolment itself is the real qa-runner.mjs over a temp data root.
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { tempRoot } from "./test-support/temp-roots.mjs";
import { carriedShapeDecision, classifyCargoArgv } from "./cargo-shapes.mjs";
import { carriedRequestOf } from "./snapshot-request.mjs";
import { enrollQaRunner, resolveManagedQaRunner } from "./qa-runner.mjs";
import { TOOL_UNITS, ToolBuildError, builtToolBinary, pinnedTargetDir, runUnitTool, toolBuildArgs, toolPlan } from "./tool-build.mjs";

const commit = "a".repeat(40), tree = "b".repeat(40);
const SHA = /^[0-9a-f]{64}$/;

test("the build argv is the one shape the broker admits as a compile and a carried call sends unchanged", () => {
  const argv = toolBuildArgs("rightkit-qa");
  assert.deepEqual(argv, ["build", "--release", "--locked", "--bin", "rightkit-qa"]);
  const row = classifyCargoArgv(argv);
  assert.equal(row.refused, undefined);
  assert.equal(row.compile, true);
  assert.deepEqual(carriedShapeDecision(argv), { ok: true, argv, compile: true });
  assert.deepEqual(carriedRequestOf(argv, "crates/rightkit-qa"), { argv, cwd: "crates/rightkit-qa" }, "no argument is added or dropped, and --locked is not doubled");
  assert.equal(classifyCargoArgv(["install", "--path", "."]).refused.code, "RK_COMMAND_DENIED", "cargo install stays denied: this route is the sanctioned alternative");
});

test("toolPlan: only a registered unit on a Mac or Windows host has a plan, and each refusal is typed", () => {
  assert.deepEqual(Object.keys(TOOL_UNITS), ["cargo:rightkit-qa"]);
  const mac = toolPlan({ selector: "cargo:rightkit-qa", platform: "darwin" });
  assert.deepEqual({ ...mac }, { selector: "cargo:rightkit-qa", crate: "rightkit-qa", bin: "rightkit-qa", enrols: "qa-runner", cargoDir: "crates/rightkit-qa", argv: toolBuildArgs("rightkit-qa"), executable: "rightkit-qa", runnerPlatform: "mac" });
  const win = toolPlan({ selector: "cargo:rightkit-qa", platform: "win32" });
  assert.equal(win.executable, "rightkit-qa.exe");
  assert.equal(win.runnerPlatform, "win");
  const code = (fn) => { try { fn(); } catch (error) { assert.ok(error instanceof ToolBuildError); return error.code; } return null; };
  assert.equal(code(() => toolPlan({ selector: "cargo:rightkit-fs", platform: "darwin" })), "RK_TOOL_UNIT_UNSUPPORTED");
  assert.equal(code(() => toolPlan({ selector: "npm:@rightkit/release", platform: "darwin" })), "RK_TOOL_UNIT_UNSUPPORTED");
  assert.equal(code(() => toolPlan({ selector: undefined, platform: "darwin" })), "RK_TOOL_UNIT_UNSUPPORTED");
  assert.equal(code(() => toolPlan({ selector: "toString", platform: "darwin" })), "RK_TOOL_UNIT_UNSUPPORTED", "an inherited property is no registered unit");
  assert.equal(code(() => toolPlan({ selector: "cargo:rightkit-qa", platform: "linux" })), "RK_TOOL_HOST");
});

test("builtToolBinary: the binary is a regular file under <outputDir>/release, never a link or a guess", (t) => {
  const dir = tempRoot("tool-bin-");
  assert.throws(() => builtToolBinary({ outputDir: undefined, executable: "rightkit-qa" }), /RK_TOOL_NO_OUTPUT_DIR/);
  assert.throws(() => builtToolBinary({ outputDir: "relative/target", executable: "rightkit-qa" }), /RK_TOOL_NO_OUTPUT_DIR/);
  assert.throws(() => builtToolBinary({ outputDir: dir, executable: "rightkit-qa" }), /RK_TOOL_NO_BINARY/);
  fs.mkdirSync(path.join(dir, "release"));
  const real = path.join(dir, "elsewhere"); fs.writeFileSync(real, "x");
  if (process.platform !== "win32") {
    fs.symlinkSync(real, path.join(dir, "release", "rightkit-qa"));
    assert.throws(() => builtToolBinary({ outputDir: dir, executable: "rightkit-qa" }), /RK_TOOL_NO_BINARY/, "a link is refused");
    fs.rmSync(path.join(dir, "release", "rightkit-qa"));
  }
  fs.writeFileSync(path.join(dir, "release", "rightkit-qa"), "bin");
  assert.equal(builtToolBinary({ outputDir: dir, executable: "rightkit-qa" }), path.join(dir, "release", "rightkit-qa"));
  void t;
});

function setup({ version = "0.10.0", platform = "darwin", reported = `rightkit-qa ${version}`, output = "direct" } = {}) {
  const root = tempRoot("tool-run-");
  const data = path.join(root, "data"); fs.mkdirSync(data);
  const target = path.join(root, "checkout", "crates", "rightkit-qa", "target"); fs.mkdirSync(path.join(target, "release"), { recursive: true });
  const exe = platform === "win32" ? "rightkit-qa.exe" : "rightkit-qa";
  const binary = path.join(target, "release", exe);
  const calls = [], outLines = [], errLines = [], reports = [], guard = [];
  const spawned = [];
  const fakeSpawn = (file, args) => { spawned.push([file, ...args]); return { status: 0, stdout: `${reported}\n`, stderr: "" }; };
  const args = {
    selector: "cargo:rightkit-qa", root: path.join(root, "snap"), commonDir: path.join(root, "common"), commit, tree, executor: { kind: "direct" }, env: { PATH: "/usr/bin" }, platform,
    envExtra: { CARGO_TARGET_DIR: target }, targetDirs: output === "direct" ? { "crates/rightkit-qa": target } : {},
    resolve: () => ({ kind: "cargo", name: "rightkit-qa", version }),
    readKnownRed: () => JSON.stringify({}),
    check: async (tier) => { calls.push(`check:${tier}`); return 0; },
    stageGuard: { start: async (stage) => { guard.push(`start:${stage}`); return true; }, end: async (stage) => { guard.push(`end:${stage}`); return true; } },
    run: async (argv, options) => {
      calls.push(["cargo", ...argv].join(" "));
      assert.equal(options.cwd, path.join(root, "snap", "crates", "rightkit-qa"), "cargo runs in the unit's own workspace of the snapshot");
      assert.ok(options.env.RK_GATE_RUN, "the build runs inside a live door run");
      assert.equal(options.env.CARGO_TARGET_DIR, target, "the stage's Cargo binding reaches the call");
      fs.writeFileSync(binary, "built-bytes", { mode: 0o755 });
      return { status: 0, stdout: "", stderr: "", ...(output === "carried" ? { outputDir: target } : {}) };
    },
    dataRoot: data,
    enroll: (a) => { calls.push("enroll"); return enrollQaRunner({ ...a, spawn: fakeSpawn, hostPlatform: platform }); },
    out: (m) => outLines.push(m), err: (m) => errLines.push(m), writeReport: (r) => reports.push(JSON.parse(JSON.stringify(r))),
  };
  fs.mkdirSync(args.commonDir, { recursive: true });
  return { args, calls, outLines, errLines, reports, guard, data, binary, target, fakeSpawn, spawned };
}

test("class A first, then the build, then the enrolment; the enrolled copy is the built bytes and its path and sha256 are printed", async () => {
  const s = setup();
  assert.equal(await runUnitTool(s.args), 0, s.errLines.join("\n"));
  assert.deepEqual(s.calls, ["check:compile", "cargo build --release --locked --bin rightkit-qa", "enroll"]);
  assert.deepEqual(s.guard, ["start:check", "end:check", "start:tool", "end:tool"], "the ledger verify brackets both stages");
  const sha = (await import("node:crypto")).createHash("sha256").update("built-bytes").digest("hex");
  const runnerFile = path.join(s.data, "qa-runners", "mac", "0.10.0", "rightkit-qa");
  assert.equal(fs.readFileSync(runnerFile, "utf8"), "built-bytes", "the managed copy holds the built bytes");
  const printed = s.outLines.join("\n");
  assert.ok(printed.includes(`${runnerFile} sha256 ${sha}`), printed);
  assert.match(printed, /right-release qa policy approve-runner --runner .*rightkit-qa --version 0\.10\.0 --platform mac --set-by <name>/);
  assert.deepEqual(s.spawned.at(-1).slice(1), ["--version"], "the enrolment executed the runner to read its version");
  // The record resolves for a source QA of that exact version.
  const resolved = resolveManagedQaRunner({ dataRoot: s.data, platform: "mac", version: "0.10.0", spawn: s.fakeSpawn });
  assert.equal(resolved.sha256, sha);
  const last = s.reports.at(-1);
  assert.equal(last.schema, "rightkit.tool-build-report/1");
  assert.equal(last.stages.check.ok && last.stages.tool.ok && last.stages.enrol.ok, true);
  assert.equal(last.stages.enrol.sha256, sha); assert.match(last.stages.tool.binarySha256, SHA);
  assert.equal(last.stages.enrol.version, "0.10.0");
});

test("the carried receipt's output directory is the one used on a broker host (no pinned target needed)", async () => {
  const s = setup({ output: "carried" });
  assert.equal(await runUnitTool(s.args), 0, s.errLines.join("\n"));
  assert.ok(s.calls.includes("enroll"));
});

test("Windows: the .exe is built and enrolled for win", async () => {
  const s = setup({ platform: "win32" });
  assert.equal(await runUnitTool(s.args), 0, s.errLines.join("\n"));
  assert.ok(fs.existsSync(path.join(s.data, "qa-runners", "win", "0.10.0", "rightkit-qa.exe")));
});

test("a failed class A check builds and enrols nothing", async () => {
  const s = setup();
  const code = await runUnitTool({ ...s.args, check: async () => 1 });
  assert.equal(code, 1);
  assert.match(s.errLines.join("\n"), /stopped at check: class A check \(compile tier\) failed/);
  assert.deepEqual(s.calls, []);
  assert.equal(fs.existsSync(path.join(s.data, "qa-runners")), false);
});

test("a failing cargo build enrols nothing and says why", async () => {
  const s = setup();
  const code = await runUnitTool({ ...s.args, run: async () => ({ status: 101, stdout: "", stderr: "error[E0432]: unresolved import" }) });
  assert.equal(code, 1);
  assert.match(s.errLines.join("\n"), /stopped at tool: .*failed \(exit 101\)[\s\S]*unresolved import/);
  assert.equal(fs.existsSync(path.join(s.data, "qa-runners")), false);
  assert.equal(s.reports.at(-1).stages.tool.ok, false);
});

test("a build that reports no output directory or leaves no binary enrols nothing", async () => {
  const s = setup({ output: "carried" });
  assert.equal(await runUnitTool({ ...s.args, targetDirs: {}, run: async () => ({ status: 0, stdout: "", stderr: "" }) }), 1);
  assert.match(s.errLines.join("\n"), /RK_TOOL_NO_OUTPUT_DIR/);
  const t = setup();
  assert.equal(await runUnitTool({ ...t.args, run: async () => ({ status: 0, stdout: "", stderr: "" }) }), 1);
  assert.match(t.errLines.join("\n"), /RK_TOOL_NO_BINARY/);
  assert.equal(fs.existsSync(path.join(t.data, "qa-runners")), false);
});

test("a ledger or snapshot problem after the build (the stage guard) fails the run before the enrolment", async () => {
  const s = setup();
  const code = await runUnitTool({ ...s.args, stageGuard: { start: async () => true, end: async (stage) => stage !== "tool" } });
  assert.equal(code, 1);
  assert.match(s.errLines.join("\n"), /the tool stage's result covers no known bytes/);
  assert.equal(s.calls.includes("enroll"), false);
});

test("the binary must answer --version as the committed version: a mismatch is refused and nothing is recorded", async () => {
  const s = setup({ reported: "rightkit-qa 0.9.9" });
  assert.equal(await runUnitTool(s.args), 1);
  assert.match(s.errLines.join("\n"), /RK_QA_RUNNER_VERSION_MISMATCH/);
  assert.equal(fs.existsSync(path.join(s.data, "qa-runners", "mac", "record.json")), false);
});

test("an already-enrolled version is not replaced silently: the refusal names the built binary, its sha256 and the deliberate rotation", async () => {
  const s = setup();
  assert.equal(await runUnitTool(s.args), 0);
  s.errLines.length = 0; s.outLines.length = 0;
  const again = await runUnitTool({ ...s.args, run: async () => { fs.writeFileSync(s.binary, "rebuilt-different", { mode: 0o755 }); return { status: 0, stdout: "", stderr: "" }; } });
  assert.equal(again, 1);
  const text = s.errLines.join("\n");
  assert.match(text, /RK_QA_RUNNER_EXISTS/);
  assert.match(text, new RegExp(`the built binary is ${s.binary.replaceAll("\\", "\\\\")} \\(sha256 [0-9a-f]{64}\\)`));
  assert.match(text, /--rotate/);
  assert.equal(fs.readFileSync(path.join(s.data, "qa-runners", "mac", "0.10.0", "rightkit-qa"), "utf8"), "built-bytes", "the enrolled runner is unchanged");
});

test("a binary changed between its digest and the enrolled copy is not trusted", async () => {
  const s = setup();
  const code = await runUnitTool({ ...s.args, enroll: (a) => { fs.writeFileSync(s.binary, "swapped", { mode: 0o755 }); return enrollQaRunner({ ...a, spawn: s.fakeSpawn, hostPlatform: "darwin" }); } });
  assert.equal(code, 1);
  assert.match(s.errLines.join("\n"), /RK_TOOL_DIGEST_DRIFT/);
});

test("a known-red unit, an unsupported unit and a non-Mac/Windows host are refused before anything runs", async () => {
  const s = setup();
  assert.equal(await runUnitTool({ ...s.args, readKnownRed: () => JSON.stringify({ macos: { "rust:rightkit-qa": "broken" } }) }), 1);
  assert.match(s.errLines.join("\n"), /known-red/);
  const t = setup();
  assert.equal(await runUnitTool({ ...t.args, selector: "cargo:rightkit-fs" }), 1);
  assert.match(t.errLines.join("\n"), /RK_TOOL_UNIT_UNSUPPORTED/);
  const u = setup();
  assert.equal(await runUnitTool({ ...u.args, platform: "linux" }), 1);
  assert.match(u.errLines.join("\n"), /RK_TOOL_HOST/);
  for (const x of [s, t, u]) assert.deepEqual(x.calls, [], "no check, build or enrolment ran");
});

test("pinnedTargetDir: the unit's own workspace, else the nearest enclosing one, else none", () => {
  assert.equal(pinnedTargetDir({ "crates/rightkit-qa": "/t/qa", ".": "/t/root" }, "crates/rightkit-qa"), "/t/qa");
  assert.equal(pinnedTargetDir({ ".": "/t/root" }, "crates/rightkit-qa"), "/t/root", "a member of the root workspace builds into the root's target");
  assert.equal(pinnedTargetDir({ "crates/rightkit-fs": "/t/fs" }, "crates/rightkit-qa"), null, "a sibling's target is never taken");
  assert.equal(pinnedTargetDir({ "crates/rightkit-qa": null }, "crates/rightkit-qa"), null, "a broker workspace pins none (the receipt names it)");
  assert.equal(pinnedTargetDir(undefined, "crates/rightkit-qa"), null);
});
