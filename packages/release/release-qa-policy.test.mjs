// ADR-0001 item 10, C4-01 and C4-22: platform-bound runner approvals (`approve-runner`) and per-stage admission modes.
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import {
  ADMISSION_STAGES, admissionModeFor, approvedRunnerDigests, approveRunner, checkPolicyTransition, loadAdmissionPolicy, main, POLICY_OWNER,
  QA_ADMISSION_POLICY_FILE, QA_POLICY_BASELINE_REF_ENV, QaPolicyError, setAdmissionMode, validateAdmissionPolicy,
} from "./release-qa-policy.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
const SHA = (c) => c.repeat(64);
const RUNNER_BYTES = "rightkit-qa runner bytes";
const RUNNER_SHA = createHash("sha256").update(RUNNER_BYTES).digest("hex");
const VERSION = "0.9.0";
const NOW = () => new Date("2026-10-24T10:00:00Z");
const EMPTY = { schema: 1, apps: {}, approvedRunners: [] };
const refuse = (set_by = "automation") => ({ mode: "refuse", since: "2026-10-01", set_by });
const warn = (set_by = "automation") => ({ mode: "warn", since: "2026-10-02", set_by });
const policyOf = (apps = {}, approvedRunners = []) => ({ schema: 1, apps, approvedRunners });
const reports = (version) => () => ({ status: 0, stdout: `rightkit-qa ${version}\n`, stderr: "" });

// A directory holding a synthetic policy file, a synthetic runner, and a synthetic published-version feed.
function withDir(body, policy = EMPTY) {
  const dir = mkdtempSync(path.join(tmpdir(), "rk-qa-policy-test-"));
  try {
    const policyFile = path.join(dir, QA_ADMISSION_POLICY_FILE);
    const runner = path.join(dir, "rightkit-qa");
    const versionsFile = path.join(dir, "rightkit-versions.json");
    writeFileSync(policyFile, JSON.stringify(policy));
    writeFileSync(runner, RUNNER_BYTES);
    writeFileSync(versionsFile, JSON.stringify({ schema: 2, registry: { cargo: { "rightkit-qa": VERSION, "rightkit-qa-driver": "0.7.0" }, npm: {} } }));
    const read = () => JSON.parse(readFileSync(policyFile, "utf8"));
    const write = (value) => writeFileSync(policyFile, JSON.stringify(value));
    return body({ dir, policyFile, runner, versionsFile, read, write });
  } finally { rmSync(dir, { recursive: true, force: true }); }
}

const approve = (ctx, overrides = {}) => approveRunner({
  runner: ctx.runner, version: VERSION, platform: "win", setBy: POLICY_OWNER, dir: ctx.dir, now: NOW, spawn: reports(VERSION), versionsFile: ctx.versionsFile, ...overrides,
});
const neverRuns = () => assert.fail("the runner must not be executed");

test("approve-runner appends a platform-bound entry that loadAdmissionPolicy accepts and approvedRunnerDigests lists", () => withDir((ctx) => {
  const calls = [];
  const entry = approve(ctx, { note: "qualified on the Dell", spawn: (...args) => { calls.push(args); return reports(VERSION)(); } });
  assert.deepEqual(entry, { sha256: RUNNER_SHA, version: VERSION, platform: "win", set_by: "adrian", since: "2026-10-24", note: "qualified on the Dell" });
  // The file that was digested is the file that ran: an absolute path, no PATH lookup.
  assert.equal(calls.length, 1);
  assert.equal(calls[0][0], path.resolve(ctx.runner));
  assert.deepEqual(calls[0][1], ["--version"]);
  assert.equal(calls[0][2].windowsHide, true);
  assert.deepEqual(ctx.read().approvedRunners, [entry]);
  const loaded = loadAdmissionPolicy({ dir: ctx.dir, baseline: false });
  assert.ok(approvedRunnerDigests(loaded.policy).includes(RUNNER_SHA));
  assert.deepEqual(validateAdmissionPolicy(loaded.policy), []);
  assert.ok(readFileSync(ctx.policyFile, "utf8").endsWith("}\n"));
  // No note, no `note` key.
  withDir((other) => assert.equal("note" in approve(other), false));
}));

test("approve-runner: re-approving adds no duplicate; another platform adds an entry; entries stay sorted", () => withDir((ctx) => {
  const first = approve(ctx);
  const bytes = readFileSync(ctx.policyFile, "utf8");
  assert.deepEqual(approve(ctx, { now: () => new Date("2027-01-01T00:00:00Z"), note: "again" }), first, "the existing entry is returned unchanged");
  assert.equal(readFileSync(ctx.policyFile, "utf8"), bytes, "and the file is not rewritten");
  const mac = approve(ctx, { platform: "mac" });
  assert.deepEqual(ctx.read().approvedRunners.map((entry) => entry.platform), ["mac", "win"]);
  assert.equal(ctx.read().approvedRunners.filter((entry) => entry.sha256 === RUNNER_SHA).length, 2);
  assert.equal(mac.platform, "mac");
  // Entries sort by sha256, then platform, whatever order they were written in.
  ctx.write(policyOf({}, [{ sha256: SHA("f") }, { sha256: SHA("0"), platform: "win" }, { sha256: SHA("0"), platform: "mac" }]));
  approve(ctx, { platform: "win" });
  const written = ctx.read().approvedRunners;
  assert.equal(written.length, 4);
  const byShaThenPlatform = (a, b) => (a.sha256 === b.sha256 ? (a.platform ?? "").localeCompare(b.platform ?? "") : a.sha256 < b.sha256 ? -1 : 1);
  assert.deepEqual(written, [...written].sort(byShaThenPlatform));
  assert.deepEqual(written.slice(0, 2).map((entry) => [entry.sha256, entry.platform]), [[SHA("0"), "mac"], [SHA("0"), "win"]]);
}));

test("approve-runner refuses a version the registry feed does not publish, and never runs the file", () => withDir((ctx) => {
  const before = readFileSync(ctx.policyFile, "utf8");
  assert.throws(() => approve(ctx, { version: "0.9.1", spawn: neverRuns }), (error) => error instanceof QaPolicyError && /rightkit-qa 0\.9\.1 is not the published version.*has 0\.9\.0/.test(error.message));
  assert.throws(() => approve(ctx, { version: "0.8.0", spawn: neverRuns }), /not the published version/);
  writeFileSync(ctx.versionsFile, JSON.stringify({ schema: 2, registry: { cargo: { "rightkit-qa-driver": "0.7.0" } } }));
  assert.throws(() => approve(ctx, { spawn: neverRuns }), /has no rightkit-qa/);
  writeFileSync(ctx.versionsFile, "{not json");
  assert.throws(() => approve(ctx, { spawn: neverRuns }), /published rightkit-qa version cannot be read/);
  assert.throws(() => approve(ctx, { versionsFile: path.join(ctx.dir, "missing.json"), spawn: neverRuns }), /cannot be read/);
  assert.equal(readFileSync(ctx.policyFile, "utf8"), before);
}));

test("approve-runner refuses when the runner's --version output does not match", () => withDir((ctx) => {
  const before = readFileSync(ctx.policyFile, "utf8");
  const said = (stdout, status = 0) => () => ({ status, stdout, stderr: "" });
  assert.throws(() => approve(ctx, { spawn: reports("0.8.0") }), (error) => error instanceof QaPolicyError && /reported "rightkit-qa 0\.8\.0".*expected "rightkit-qa 0\.9\.0"/.test(error.message));
  assert.throws(() => approve(ctx, { spawn: said("rightkit-qa 0.9.0 extra\n") }), /expected "rightkit-qa 0\.9\.0"/);
  assert.throws(() => approve(ctx, { spawn: said("some-other-tool 0.9.0\n") }), /expected "rightkit-qa 0\.9\.0"/);
  assert.throws(() => approve(ctx, { spawn: said("rightkit-qa 0.9.0\n", 1) }), /\(exit 1\)/);
  assert.throws(() => approve(ctx, { spawn: () => ({ status: null, stdout: "", error: new Error("spawn EACCES") }) }), /spawn EACCES/);
  assert.throws(() => approve(ctx, { spawn: () => undefined }), /expected "rightkit-qa 0\.9\.0"/);
  assert.equal(readFileSync(ctx.policyFile, "utf8"), before);
  // Output with surrounding whitespace is the same output.
  assert.equal(approve(ctx, { spawn: said("  rightkit-qa 0.9.0\r\n") }).version, VERSION);
}));

test("approve-runner refuses a runner that changes while its version is checked", () => withDir((ctx) => {
  assert.throws(() => approve(ctx, { spawn: () => { writeFileSync(ctx.runner, "swapped"); return reports(VERSION)(); } }), /changed while its version was being checked/);
  assert.deepEqual(ctx.read().approvedRunners, []);
}));

test("approve-runner refuses a set_by other than adrian, an unknown platform, bad arguments and a missing runner file", () => withDir((ctx) => {
  const before = readFileSync(ctx.policyFile, "utf8");
  for (const setBy of ["automation", "Adrian", "someone-else", "", undefined]) {
    assert.throws(() => approve(ctx, { setBy, spawn: neverRuns }), (error) => error instanceof QaPolicyError && /only adrian may approve one/.test(error.message), String(setBy));
  }
  for (const platform of ["linux", "macos", "windows", "MAC", "", undefined]) {
    assert.throws(() => approve(ctx, { platform, spawn: neverRuns }), /platform must be mac or win/, String(platform));
  }
  assert.throws(() => approve(ctx, { runner: "", spawn: neverRuns }), /runner is required/);
  assert.throws(() => approve(ctx, { version: " ", spawn: neverRuns }), /version is required/);
  assert.throws(() => approve(ctx, { note: 5, spawn: neverRuns }), /note must be a string/);
  assert.throws(() => approve(ctx, { runner: path.join(ctx.dir, "nope"), spawn: neverRuns }), /runner file is unreadable/);
  assert.throws(() => approve(ctx, { runner: ctx.dir, spawn: neverRuns }), /runner file is unreadable/);
  assert.equal(readFileSync(ctx.policyFile, "utf8"), before);
}));

test("the shipped feed publishes rightkit-qa under registry.cargo, which is what approve-runner reads by default", () => {
  const feed = JSON.parse(readFileSync(path.join(here, "rightkit-versions.json"), "utf8"));
  assert.match(feed.registry?.cargo?.["rightkit-qa"] ?? "", /^\d+\.\d+\.\d+/);
});

test("runner approvals: a hand-added entry without set_by adrian is refused against the baseline", () => withDir((ctx) => {
  const baseline = JSON.stringify(policyOf({}, [{ sha256: SHA("1"), version: "0.8.0" }]));
  const load = () => loadAdmissionPolicy({ dir: ctx.dir, baseline });
  const old = { sha256: SHA("1"), version: "0.8.0" };
  const added = (extra) => policyOf({}, [old, { sha256: SHA("2"), version: "0.9.0", platform: "mac", since: "2026-10-24", ...extra }]);
  ctx.write(added({ set_by: "automation" }));
  assert.throws(load, (error) => error instanceof QaPolicyError && /weakens its baseline.*approvedRunners: runner 2{16} \(mac\) is new with set_by "automation"/.test(error.message));
  ctx.write(added({}));
  assert.throws(load, /is new with set_by null/);
  ctx.write(added({ set_by: "Adrian" }));
  assert.throws(load, /only adrian may approve a runner/);
  ctx.write(added({ set_by: "adrian" }));
  assert.equal(load().baseline, "supplied");
  // The same digest for a platform the baseline did not approve it for is a new approval too.
  ctx.write(policyOf({}, [old, { ...old, platform: "win", set_by: "automation" }]));
  assert.throws(load, /runner 1{16} \(win\) is new/);
  // An entry the baseline already holds (even a legacy one with no set_by) is not new; removing one is allowed.
  ctx.write(policyOf({}, [old]));
  assert.equal(load().baseline, "supplied");
  ctx.write(policyOf({}, []));
  assert.equal(load().baseline, "supplied");
  assert.deepEqual(checkPolicyTransition(policyOf({}, [old]), policyOf({}, [])), []);
}));

test("runner entries are validated: sha256 required, version, platform, set_by, since and note checked when present", () => {
  const entry = { sha256: SHA("a"), version: "0.9.0", platform: "win", set_by: "adrian", since: "2026-10-24", note: "n" };
  assert.deepEqual(validateAdmissionPolicy(policyOf({}, [entry])), []);
  assert.deepEqual(validateAdmissionPolicy(policyOf({}, [{ sha256: SHA("a") }])), [], "legacy entries stay valid");
  const problems = (change) => validateAdmissionPolicy(policyOf({}, [{ ...entry, ...change }])).join(";");
  assert.match(problems({ platform: "linux" }), /approvedRunners entry a{16} platform must be mac or win/);
  assert.match(problems({ platform: "" }), /platform must be mac or win/);
  assert.match(problems({ version: "" }), /version must be a non-empty string/);
  assert.match(problems({ version: 9 }), /version must be a non-empty string/);
  assert.match(problems({ set_by: " " }), /set_by must be a non-empty string/);
  assert.match(problems({ since: "24/10/2026" }), /since must be YYYY-MM-DD/);
  assert.match(problems({ note: 3 }), /note must be a string/);
  assert.match(validateAdmissionPolicy(policyOf({}, [{ sha256: SHA("A") }])).join(";"), /lowercase 64-hex sha256/);
  assert.match(validateAdmissionPolicy(policyOf({}, [{ version: "0.9.0" }])).join(";"), /lowercase 64-hex sha256/);
});

test("per-stage modes: a stage entry overrides the app-level mode, else the app-level mode applies, else warn", () => {
  const policy = policyOf({
    both: refuse("adrian"),
    split: { ...warn("adrian"), stages: { source: refuse("adrian") } },
    onlyInstalled: { stages: { installed: refuse("adrian") } },
  });
  assert.deepEqual(ADMISSION_STAGES, ["source", "installed"]);
  assert.equal(admissionModeFor(policy, "both", "source").mode, "refuse");
  assert.equal(admissionModeFor(policy, "both", "installed").mode, "refuse");
  assert.equal(admissionModeFor(policy, "split", "source").mode, "refuse");
  assert.equal(admissionModeFor(policy, "split", "installed").mode, "warn");
  assert.equal(admissionModeFor(policy, "onlyInstalled", "installed").mode, "refuse");
  assert.deepEqual(admissionModeFor(policy, "onlyInstalled", "source"), { mode: "warn", since: null, set_by: null });
  assert.deepEqual(admissionModeFor(policy, "missing", "source"), { mode: "warn", since: null, set_by: null });
  assert.deepEqual(admissionModeFor(policy, "split", "source"), { mode: "refuse", since: "2026-10-01", set_by: "adrian" });
  // A stage entry can be warn under an app-level refuse.
  const relaxed = policyOf({ app: { ...refuse("adrian"), stages: { installed: warn("adrian") } } });
  assert.equal(admissionModeFor(relaxed, "app", "source").mode, "refuse");
  assert.equal(admissionModeFor(relaxed, "app", "installed").mode, "warn");
  // Without a stage it is the app-level result, as before.
  assert.deepEqual(admissionModeFor(policy, "both"), { mode: "refuse", since: "2026-10-01", set_by: "adrian" });
  assert.deepEqual(admissionModeFor(policy, "split"), { mode: "warn", since: "2026-10-02", set_by: "adrian" });
  assert.deepEqual(admissionModeFor(policy, "onlyInstalled"), { mode: "warn", since: null, set_by: null });
  assert.deepEqual(admissionModeFor(policy, "missing"), { mode: "warn", since: null, set_by: null });
});

test("per-stage validation: unknown stage keys, empty entries and incomplete stage entries are refused", () => {
  const problems = (entry) => validateAdmissionPolicy(policyOf({ demo: entry })).join(";");
  assert.deepEqual(validateAdmissionPolicy(policyOf({ demo: { stages: { source: refuse() } } })), [], "an app governed by its stages alone is valid");
  assert.deepEqual(validateAdmissionPolicy(policyOf({ demo: { ...warn(), stages: { source: refuse(), installed: warn() } } })), []);
  assert.match(problems({ stages: { build: refuse() } }), /policy app demo has unknown stage "build" \(expected source or installed\)/);
  assert.match(problems({ ...refuse(), stages: { Source: refuse() } }), /unknown stage "Source"/);
  assert.match(problems({}), /mode must be warn or refuse/);
  assert.match(problems({ stages: {} }), /needs a mode or a non-empty stages object/);
  assert.deepEqual(validateAdmissionPolicy(policyOf({ demo: { ...refuse(), stages: {} } })), [], "a mode with no stage entries is the old form");
  assert.match(problems({ stages: [] }), /stages must be an object/);
  assert.match(problems({ stages: { source: null } }), /stage source must be an object/);
  assert.match(problems({ stages: { source: { mode: "maybe", since: "x", set_by: "" } } }), /stage source mode must be warn or refuse.*stage source since must be YYYY-MM-DD.*stage source set_by is required/);
  assert.match(problems({ mode: "maybe", since: "x", set_by: "", stages: { installed: refuse() } }), /policy app demo mode must be warn or refuse/);
});

test("set --stage writes only that stage; set without a stage writes the app-level entry and removes the stages", () => withDir((ctx) => {
  assert.deepEqual(setAdmissionMode({ app: "demo", mode: "refuse", setBy: "automation", stage: "source", dir: ctx.dir, now: NOW }), { mode: "refuse", since: "2026-10-24", set_by: "automation" });
  assert.deepEqual(ctx.read().apps.demo, { stages: { source: { mode: "refuse", since: "2026-10-24", set_by: "automation" } } });
  setAdmissionMode({ app: "demo", mode: "warn", setBy: "automation", stage: "installed", dir: ctx.dir, now: NOW });
  assert.deepEqual(Object.keys(ctx.read().apps.demo.stages), ["source", "installed"]);
  const loaded = loadAdmissionPolicy({ dir: ctx.dir, baseline: false }).policy;
  assert.equal(admissionModeFor(loaded, "demo", "source").mode, "refuse");
  assert.equal(admissionModeFor(loaded, "demo", "installed").mode, "warn");
  // Other fields are kept: an app-level mode and the other stage survive a stage write.
  ctx.write(policyOf({ demo: { ...warn(), stages: { installed: refuse() } } }));
  setAdmissionMode({ app: "demo", mode: "refuse", setBy: "automation", stage: "source", dir: ctx.dir, now: NOW });
  assert.deepEqual(ctx.read().apps.demo, { ...warn(), stages: { source: { mode: "refuse", since: "2026-10-24", set_by: "automation" }, installed: refuse() } });
  // Without a stage, one mode governs both stages again.
  setAdmissionMode({ app: "demo", mode: "refuse", setBy: "automation", dir: ctx.dir, now: NOW });
  assert.deepEqual(ctx.read().apps.demo, { mode: "refuse", since: "2026-10-24", set_by: "automation" });
  assert.throws(() => setAdmissionMode({ app: "demo", mode: "refuse", setBy: "x", stage: "build", dir: ctx.dir }), /stage must be source or installed/);
  assert.throws(() => setAdmissionMode({ app: "demo", mode: "refuse", setBy: "x", stage: "", dir: ctx.dir }), /stage must be source or installed/);
}));

test("refuse -> warn for one stage is refused without adrian and accepted with adrian; the other stage keeps refusing", () => withDir((ctx) => {
  ctx.write(policyOf({ demo: { stages: { source: refuse(), installed: refuse() } } }));
  for (const setBy of ["automation", "Adrian", "someone-else"]) {
    assert.throws(() => setAdmissionMode({ app: "demo", mode: "warn", setBy, stage: "source", dir: ctx.dir }), /demo \(source\): refuse -> warn requires set_by "adrian"/);
  }
  assert.equal(ctx.read().apps.demo.stages.source.mode, "refuse", "a refused change leaves the file untouched");
  assert.equal(setAdmissionMode({ app: "demo", mode: "warn", setBy: "adrian", stage: "source", dir: ctx.dir }).mode, "warn");
  const policy = loadAdmissionPolicy({ dir: ctx.dir, baseline: false }).policy;
  assert.equal(admissionModeFor(policy, "demo", "source").mode, "warn");
  assert.equal(admissionModeFor(policy, "demo", "installed").mode, "refuse");
  // A stage entry that relaxes an app-level refuse is the same downgrade.
  ctx.write(policyOf({ demo: refuse() }));
  assert.throws(() => setAdmissionMode({ app: "demo", mode: "warn", setBy: "automation", stage: "installed", dir: ctx.dir }), /demo \(installed\): refuse -> warn requires set_by "adrian"/);
  assert.equal(setAdmissionMode({ app: "demo", mode: "warn", setBy: "adrian", stage: "installed", dir: ctx.dir }).mode, "warn");
  assert.equal(admissionModeFor(ctx.read(), "demo", "source").mode, "refuse");
  // The transition rule covers the whole result: an app-level warn without a stage also drops the stage refuse.
  ctx.write(policyOf({ demo: { ...warn("adrian"), stages: { source: refuse() } } }));
  assert.throws(() => setAdmissionMode({ app: "demo", mode: "warn", setBy: "automation", dir: ctx.dir }), /demo \(source\): refuse -> warn/);
  assert.equal(setAdmissionMode({ app: "demo", mode: "warn", setBy: "adrian", dir: ctx.dir }).set_by, "adrian");
  assert.deepEqual(ctx.read().apps.demo.stages, undefined);
  // warn -> refuse needs no owner.
  assert.equal(setAdmissionMode({ app: "demo", mode: "refuse", setBy: "automation", stage: "source", dir: ctx.dir }).mode, "refuse");
}));

test("removing an app (or a stage entry) whose stage was refuse is refused, against the baseline too", () => withDir((ctx) => {
  const previous = policyOf({ demo: { ...warn("adrian"), stages: { source: refuse() } }, other: warn() });
  const problems = checkPolicyTransition(previous, policyOf({ other: warn() }));
  assert.equal(problems.length, 1);
  assert.match(problems[0], /^demo \(source\): the refuse entry was removed; only adrian may move an app back to warn/);
  // The stage entry removed while the app-level mode is warn: the stage falls back to that warn, so it needs adrian.
  assert.match(checkPolicyTransition(previous, policyOf({ demo: warn(), other: warn() })).join(";"), /demo \(source\): refuse -> warn requires set_by "adrian", got "automation"/);
  assert.deepEqual(checkPolicyTransition(previous, policyOf({ demo: warn("adrian"), other: warn() })), []);
  // The stage entry removed while the app-level mode is refuse: still refuse, nothing loosened.
  const staged = policyOf({ demo: { ...refuse(), stages: { installed: refuse() } } });
  assert.deepEqual(checkPolicyTransition(staged, policyOf({ demo: refuse() })), []);
  // An app that was refuse at both stages and is gone names both stages in one message.
  assert.deepEqual(checkPolicyTransition(policyOf({ demo: refuse() }), EMPTY), [`demo (source, installed): the refuse entry was removed; only adrian may move an app back to warn (set mode warn with set_by "adrian")`]);
  // Warn entries (either form) are never a downgrade to remove.
  assert.deepEqual(checkPolicyTransition(policyOf({ a: warn(), b: { stages: { source: warn() } } }), EMPTY), []);
  // On load against a baseline.
  const baseline = JSON.stringify(previous);
  ctx.write(policyOf({ other: warn() }));
  assert.throws(() => loadAdmissionPolicy({ dir: ctx.dir, baseline }), /weakens its baseline.*demo \(source\): the refuse entry was removed/);
  ctx.write(policyOf({ demo: { ...warn("adrian"), stages: { source: warn("adrian") } }, other: warn() }));
  assert.equal(loadAdmissionPolicy({ dir: ctx.dir, baseline }).baseline, "supplied");
}));

test("loadAdmissionPolicy fails closed when an explicitly requested baseline ref cannot be read", () => withDir((ctx) => {
  const calls = [];
  const failing = (command, args) => { calls.push([command, args]); return { status: 128, stdout: "", stderr: "fatal: invalid object name 'origin/main'.\n" }; };
  assert.throws(() => loadAdmissionPolicy({ dir: ctx.dir, baselineRef: "origin/main", env: {}, gitRunner: failing }),
    (error) => error instanceof QaPolicyError && /baseline origin\/main cannot be read \(fatal: invalid object name 'origin\/main'\.\)/.test(error.message));
  assert.equal(calls[0][0], "git");
  assert.deepEqual(calls[0][1], ["-C", ctx.dir, "show", `origin/main:./${QA_ADMISSION_POLICY_FILE}`]);
  // The environment variable is an explicit request too, and so is a runner that throws.
  assert.throws(() => loadAdmissionPolicy({ dir: ctx.dir, env: { [QA_POLICY_BASELINE_REF_ENV]: "release/protected" }, gitRunner: failing }), /baseline release\/protected cannot be read/);
  assert.throws(() => loadAdmissionPolicy({ dir: ctx.dir, baselineRef: "origin/main", env: {}, gitRunner: () => { throw new Error("git not found"); } }), /baseline origin\/main cannot be read \(git not found\)/);
  assert.throws(() => loadAdmissionPolicy({ dir: ctx.dir, baselineRef: "origin/main", env: {}, gitRunner: () => ({ status: 1, stdout: "", stderr: "" }) }), /cannot be read \(git exited 1\)/);
  // The argument wins over the environment, and a readable explicit ref is used.
  const ok = (command, args) => { calls.push(args.at(-1)); return { status: 0, stdout: JSON.stringify(EMPTY), stderr: "" }; };
  calls.length = 0;
  assert.equal(loadAdmissionPolicy({ dir: ctx.dir, baselineRef: "origin/main", env: { [QA_POLICY_BASELINE_REF_ENV]: "elsewhere" }, gitRunner: ok }).baseline, "git");
  assert.deepEqual(calls, [`origin/main:./${QA_ADMISSION_POLICY_FILE}`]);
  // A readable baseline is still compared: the explicit ref cannot be used to skip the transition rule.
  const strict = () => ({ status: 0, stdout: JSON.stringify(policyOf({ demo: refuse("adrian") })), stderr: "" });
  assert.throws(() => loadAdmissionPolicy({ dir: ctx.dir, baselineRef: "origin/main", env: {}, gitRunner: strict }), /weakens its baseline/);
  // `baseline: false` and a supplied baseline never consult git.
  assert.equal(loadAdmissionPolicy({ dir: ctx.dir, baselineRef: "origin/main", baseline: false, env: {}, gitRunner: neverRuns }).baseline, "none");
  assert.equal(loadAdmissionPolicy({ dir: ctx.dir, baselineRef: "origin/main", baseline: JSON.stringify(EMPTY), env: {}, gitRunner: neverRuns }).baseline, "supplied");
}));

test("loadAdmissionPolicy with no ref requested keeps the default: HEAD, and no baseline when git cannot show it", () => withDir((ctx) => {
  const calls = [];
  const failing = (command, args) => { calls.push(args.at(-1)); return { status: 128, stdout: "", stderr: "fatal: not a git repository" }; };
  assert.equal(loadAdmissionPolicy({ dir: ctx.dir, env: {}, gitRunner: failing }).baseline, "none");
  assert.equal(loadAdmissionPolicy({ dir: ctx.dir, env: {}, gitRunner: () => { throw new Error("git not found"); } }).baseline, "none");
  // An empty or blank request is no request.
  assert.equal(loadAdmissionPolicy({ dir: ctx.dir, baselineRef: "", env: { [QA_POLICY_BASELINE_REF_ENV]: "  " }, gitRunner: failing }).baseline, "none");
  assert.deepEqual(calls, [`HEAD:./${QA_ADMISSION_POLICY_FILE}`, `HEAD:./${QA_ADMISSION_POLICY_FILE}`]);
  const ok = () => ({ status: 0, stdout: JSON.stringify(EMPTY), stderr: "" });
  assert.equal(loadAdmissionPolicy({ dir: ctx.dir, env: {}, gitRunner: ok }).baseline, "git");
}));

test("main: show, set (with and without --stage), approve-runner, usage and failures map to exit codes", () => withDir((ctx) => {
  const out = [], err = [];
  const run = (args, extra = {}) => { out.length = 0; err.length = 0; return main(args, { stdout: (text) => out.push(text), stderr: (text) => err.push(text), dir: ctx.dir, env: {}, now: NOW, versionsFile: ctx.versionsFile, spawn: reports(VERSION), ...extra }); };

  assert.equal(run(["show"]), 0);
  assert.deepEqual(JSON.parse(out[0]), EMPTY);

  assert.equal(run(["set", "--app", "demo", "--mode", "refuse", "--set-by", "automation"]), 0);
  assert.equal(out[0], "qa admission: demo -> refuse (since 2026-10-24, set_by automation)");
  assert.equal(run(["set", "--app", "demo", "--mode", "warn", "--set-by", "adrian", "--stage", "installed"]), 0);
  assert.equal(out[0], "qa admission: demo (installed stage) -> warn (since 2026-10-24, set_by adrian)");
  assert.deepEqual(Object.keys(ctx.read().apps.demo), ["mode", "since", "set_by", "stages"]);
  assert.equal(admissionModeFor(ctx.read(), "demo", "installed").mode, "warn");
  assert.equal(admissionModeFor(ctx.read(), "demo", "source").mode, "refuse");

  assert.equal(run(["set", "--app", "demo", "--mode", "warn", "--set-by", "automation", "--stage", "source"]), 1);
  assert.match(err[0], /^release-qa-policy: QA admission policy change refused: demo \(source\): refuse -> warn requires set_by "adrian"/);
  assert.equal(run(["set", "--app", "demo", "--mode", "maybe", "--set-by", "adrian"]), 1);
  assert.match(err[0], /mode must be warn or refuse/);
  assert.equal(run(["set", "--app", "demo", "--mode", "warn", "--set-by", "adrian", "--stage", "build"]), 1);
  assert.match(err[0], /stage must be source or installed/);
  // `--stage` with no value is an error, not a silent app-level write that would drop the stages.
  assert.equal(run(["set", "--app", "demo", "--mode", "warn", "--set-by", "adrian", "--stage"]), 1);
  assert.match(err[0], /stage must be source or installed/);
  assert.ok(ctx.read().apps.demo.stages, "the stages survived");

  const approveArgs = ["approve-runner", "--runner", ctx.runner, "--version", VERSION, "--platform", "mac", "--set-by", "adrian", "--note", "dell"];
  assert.equal(run(approveArgs), 0);
  assert.equal(out[0], `qa runner approved: ${RUNNER_SHA} rightkit-qa ${VERSION} (mac, since 2026-10-24, set_by adrian)`);
  assert.deepEqual(ctx.read().approvedRunners.map((entry) => [entry.platform, entry.note]), [["mac", "dell"]]);
  assert.equal(run(approveArgs), 0, "approving again succeeds without a second entry");
  assert.equal(ctx.read().approvedRunners.length, 1);
  assert.equal(run(approveArgs.map((arg) => (arg === "adrian" ? "automation" : arg))), 1);
  assert.match(err[0], /only adrian may approve one/);
  assert.equal(run(approveArgs, { spawn: reports("0.8.0") }), 1);
  assert.match(err[0], /expected "rightkit-qa 0\.9\.0"/);
  assert.equal(run(approveArgs.map((arg) => (arg === "mac" ? "linux" : arg))), 1);
  assert.match(err[0], /platform must be mac or win/);
  assert.equal(run(["approve-runner"]), 1);

  assert.equal(run([]), 2);
  assert.match(err[0], /^usage: release-qa-policy\.mjs show\n.*set --app <app> --mode warn\|refuse --set-by <name> \[--stage source\|installed\]\n.*approve-runner --runner <file> --version <v> --platform mac\|win --set-by <name> \[--note <text>\]$/s);
  assert.equal(run(["bogus"]), 2);
  assert.equal(run(["bogus"], { program: "right-release qa policy" }), 2);
  assert.match(err[0], /^usage: right-release qa policy show/);

  writeFileSync(ctx.policyFile, "{not json");
  assert.equal(run(["show"]), 1);
  assert.match(err[0], /policy is unreadable/);
}));
