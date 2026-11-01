import assert from "node:assert/strict";
import { chmodSync, mkdtempSync, readFileSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import {
  QA_RUNNERS_SCHEMA, QaRunnerError, enrollQaRunner, isRunnerVersion, main, qaRunnerFile, qaRunnersRecordFile, resolveManagedQaRunner,
} from "./qa-runner.mjs";

// Enrolling a Mac runner reads POSIX exec bits from the file; a Windows file system holds none.
const noModeBits = process.platform === "win32" ? "a Windows file system holds no POSIX mode bits (the Mac enrolment reads them)" : false;

const VERSION = "0.10.0";
const reports = (version) => () => ({ status: 0, stdout: `rightkit-qa ${version}\n`, stderr: "" });
const neverRuns = () => assert.fail("the runner must not be executed");

function withRoot(body) {
  const dir = mkdtempSync(path.join(tmpdir(), "rk-qa-runner-"));
  try {
    const dataRoot = path.join(dir, "data");
    const built = path.join(dir, "built-rightkit-qa");
    writeFileSync(built, "#!/bin/sh\necho built-bytes\n");
    chmodSync(built, 0o755);
    return body({ dir, dataRoot, built });
  } finally { rmSync(dir, { recursive: true, force: true }); }
}
const enroll = (ctx, overrides = {}) => enrollQaRunner({ dataRoot: ctx.dataRoot, platform: "mac", runner: ctx.built, version: VERSION, spawn: reports(VERSION), hostPlatform: "darwin", ...overrides });
const code = (expected) => (error) => error instanceof QaRunnerError && error.code === expected;

test("isRunnerVersion: a rightkit-qa version is major.minor.patch, the legacy plan version is not", () => {
  assert.equal(isRunnerVersion("0.10.0"), true);
  assert.equal(isRunnerVersion("1"), false);
  assert.equal(isRunnerVersion(1), false);
  assert.equal(isRunnerVersion("0.10"), false);
  assert.equal(isRunnerVersion("v0.10.0"), false);
});

test("enrol copies the built runner into the managed directory and records its digest; the copy is what resolves", { skip: noModeBits }, () => withRoot((ctx) => {
  const calls = [];
  const result = enroll(ctx, { spawn: (...a) => { calls.push(a); return reports(VERSION)(); } });
  assert.equal(result.runnerFile, qaRunnerFile({ dataRoot: ctx.dataRoot, platform: "mac", version: VERSION }));
  assert.notEqual(result.runnerFile, ctx.built, "the managed file is a copy, so a clean of the build output cannot change what is digested");
  assert.equal(readFileSync(result.runnerFile, "utf8"), readFileSync(ctx.built, "utf8"));
  assert.equal(calls.length, 2, "the source and the copy each answered --version");
  assert.deepEqual(calls[0][1], ["--version"]);
  const record = JSON.parse(readFileSync(qaRunnersRecordFile({ dataRoot: ctx.dataRoot, platform: "mac" }), "utf8"));
  assert.equal(record.schema, QA_RUNNERS_SCHEMA);
  assert.equal(record.platform, "mac");
  assert.deepEqual(record.runners[VERSION], result.entry);
  const resolved = resolveManagedQaRunner({ dataRoot: ctx.dataRoot, platform: "mac", version: VERSION, spawn: reports(VERSION) });
  assert.deepEqual(resolved, { file: result.runnerFile, sha256: result.entry.sha256, version: VERSION, source: "managed" });
  assert.match(resolved.sha256, /^[0-9a-f]{64}$/);
}));

test("enrol refuses a version the file does not report, a relative path, another platform's host, and a repeat without rotate", { skip: noModeBits }, () => withRoot((ctx) => {
  assert.throws(() => enroll(ctx, { spawn: reports("0.9.1") }), code("RK_QA_RUNNER_VERSION_MISMATCH"));
  assert.throws(() => enroll(ctx, { spawn: () => ({ status: 1, stdout: "", stderr: "x" }) }), code("RK_QA_RUNNER_VERSION_MISMATCH"));
  assert.throws(() => enroll(ctx, { runner: "relative/rightkit-qa", spawn: neverRuns }), code("RK_QA_RUNNER_PATH"));
  assert.throws(() => enroll(ctx, { runner: path.join(ctx.dir, "missing"), spawn: neverRuns }), code("RK_QA_RUNNER_PATH"));
  assert.throws(() => enroll(ctx, { hostPlatform: "win32", spawn: neverRuns }), code("RK_QA_RUNNER_PLATFORM"));
  assert.throws(() => enroll(ctx, { platform: "linux", spawn: neverRuns }), code("RK_QA_RUNNER_PLATFORM"));
  assert.throws(() => enroll(ctx, { version: "latest", spawn: neverRuns }), code("RK_QA_RUNNER_VERSION"));
  const notExecutable = path.join(ctx.dir, "plain");
  writeFileSync(notExecutable, "x");
  assert.throws(() => enroll(ctx, { runner: notExecutable, spawn: neverRuns }), code("RK_QA_RUNNER_PATH"));
  assert.throws(() => resolveManagedQaRunner({ dataRoot: ctx.dataRoot, platform: "mac", version: VERSION, spawn: neverRuns }), code("RK_QA_RUNNER_MISSING"), "nothing was written by the refusals");
  enroll(ctx);
  assert.throws(() => enroll(ctx), code("RK_QA_RUNNER_EXISTS"));
  const rotated = enroll(ctx, { rotate: true });
  assert.equal(rotated.rotated, true);
}));

test("resolve refuses with no enrolment, naming the operation that makes one, and never falls back to PATH", { skip: noModeBits }, () => withRoot((ctx) => {
  assert.throws(() => resolveManagedQaRunner({ dataRoot: ctx.dataRoot, platform: "mac", version: VERSION, spawn: neverRuns }), (error) => error.code === "RK_QA_RUNNER_MISSING" && /right-release qa runner enroll/.test(error.message) && /0\.10\.0/.test(error.message));
  assert.throws(() => resolveManagedQaRunner({ dataRoot: ctx.dataRoot, platform: "mac", version: "1", spawn: neverRuns }), code("RK_QA_RUNNER_VERSION"));
  assert.throws(() => resolveManagedQaRunner({ dataRoot: "relative", platform: "mac", version: VERSION, spawn: neverRuns }), code("RK_QA_RUNNER_ROOT"));
  enroll(ctx);
  assert.throws(() => resolveManagedQaRunner({ dataRoot: ctx.dataRoot, platform: "mac", version: "0.10.1", spawn: neverRuns }), code("RK_QA_RUNNER_MISSING"), "another version is not enrolled");
  assert.throws(() => resolveManagedQaRunner({ dataRoot: ctx.dataRoot, platform: "win", version: VERSION, spawn: neverRuns }), code("RK_QA_RUNNER_MISSING"), "another platform is not enrolled");
}));

test("resolve refuses a managed file that changed, was replaced by a link, or no longer reports its version", { skip: noModeBits }, () => withRoot((ctx) => {
  const { runnerFile } = enroll(ctx);
  const use = (spawn = reports(VERSION)) => resolveManagedQaRunner({ dataRoot: ctx.dataRoot, platform: "mac", version: VERSION, spawn });
  assert.throws(() => use(reports("0.9.1")), code("RK_QA_RUNNER_VERSION_MISMATCH"));
  writeFileSync(runnerFile, "#!/bin/sh\necho tampered\n");
  assert.throws(() => use(neverRuns), code("RK_QA_RUNNER_DIGEST"), "a changed file is refused before it is run");
  rmSync(runnerFile);
  assert.throws(() => use(neverRuns), code("RK_QA_RUNNER_MISSING"));
  symlinkSync(ctx.built, runnerFile);
  assert.throws(() => use(neverRuns), code("RK_QA_RUNNER_RECORD"), "a link is not the managed copy");
}));

test("resolve refuses a record that names a file outside the managed copy path, or is writable by others", { skip: noModeBits }, () => withRoot((ctx) => {
  enroll(ctx);
  const file = qaRunnersRecordFile({ dataRoot: ctx.dataRoot, platform: "mac" });
  const record = JSON.parse(readFileSync(file, "utf8"));
  record.runners[VERSION].path = ctx.built;
  writeFileSync(file, JSON.stringify(record));
  assert.throws(() => resolveManagedQaRunner({ dataRoot: ctx.dataRoot, platform: "mac", version: VERSION, spawn: neverRuns }), code("RK_QA_RUNNER_RECORD"));
  record.runners[VERSION].path = qaRunnerFile({ dataRoot: ctx.dataRoot, platform: "mac", version: VERSION });
  writeFileSync(file, JSON.stringify(record));
  chmodSync(file, 0o666);
  assert.throws(() => resolveManagedQaRunner({ dataRoot: ctx.dataRoot, platform: "mac", version: VERSION, spawn: neverRuns }), /writable by group or others/);
  chmodSync(file, 0o600);
  writeFileSync(file, "{not json");
  assert.throws(() => resolveManagedQaRunner({ dataRoot: ctx.dataRoot, platform: "mac", version: VERSION, spawn: neverRuns }), code("RK_QA_RUNNER_RECORD"));
}));

test("the CLI enrols with --data-root and shows the record; usage errors exit 2", () => withRoot((ctx) => {
  const out = []; const err = [];
  const io = { out: (m) => out.push(m), err: (m) => err.push(m), spawn: reports(VERSION) };
  // The CLI enrols on the host it runs on: the host-platform argument is the real one, so it is exercised only where it is true.
  if (process.platform === "darwin") {
    assert.equal(main(["enroll", "--data-root", ctx.dataRoot, "--platform", "mac", "--runner", ctx.built, "--version", VERSION], io), 0);
    assert.match(out[0], /enrolled rightkit-qa 0\.10\.0 for mac \(sha256 [0-9a-f]{64}\).*approve-runner/);
    assert.equal(main(["show", "--data-root", ctx.dataRoot, "--platform", "mac"], io), 0);
    assert.equal(JSON.parse(out[1]).runners[VERSION].version, VERSION);
  }
  assert.equal(main(["show", "--data-root", ctx.dataRoot, "--platform", "win"], io), 1);
  assert.equal(main(["frobnicate"], io), 2);
  assert.match(err.at(-1), /usage:/);
  assert.equal(main(["enroll", "--data-root", ctx.dataRoot, "--platform", "mac", "--runner", ctx.built, "--version", "x"], io), 1);
}));
