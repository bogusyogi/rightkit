import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { ADMISSION_SCHEMA, QUALIFICATION_SCHEMA, admissionArtifactName, admit, bindPackedToQualification, dispatchHosted, gateVerdict, readQualificationInput, verifyAdmissionReceipt } from "./publish-admission.mjs";
import { main as rightkitDispatch } from "./rightkit-dispatch.mjs";

const SHA = "a".repeat(40);
const OTHER = "b".repeat(40);
const NONE = { allow_host_exclusion: [], allow_no_test: [], allow_ignored: [], allow_missing_assets: [] };
const policy = (granted = NONE) => () => granted;
const ACTIONS = { GITHUB_ACTIONS: "true", GITHUB_REPOSITORY: "bogusyogi/rightkit", GITHUB_REF: "refs/heads/main", GITHUB_SHA: SHA, GITHUB_RUN_ID: "77", GITHUB_RUN_ATTEMPT: "1" };
/** rightkit-gate's qualification receipt as the dispatcher forwards it (one unit, every local stage ok, one artifact sha). */
const qual = ({ kind = "npm", name = "@rightkit/release", version = "0.6.7", commit = SHA, ...over } = {}) => ({ schema: QUALIFICATION_SCHEMA, commit, tree: "c".repeat(40), runtime: "r".repeat(64), executor: "broker",
  unit: { kind, name, version }, stages: { check: { ok: true }, compile: { ok: true }, package: { ok: true }, qualify: { ok: true } }, artifactSha256: "d".repeat(64), ...over });
const QUAL = { schema: QUALIFICATION_SCHEMA, commit: SHA, tree: "c".repeat(40), runtime: "r".repeat(64), executor: "broker", unit: { kind: "npm", name: "@rightkit/release", version: "0.6.7" }, artifactSha256: "d".repeat(64) };
/** The only admitted shape: the broker's dispatch of rightkit-gate --to release, carrying the qualification for the first unit named. */
const brokerEnv = (extra = {}) => {
  const env = { ...ACTIONS, ADMISSION: "broker", REQUEST_ID: "req-1", EXPECTED_SHA: SHA, REGISTRY: "npm", UNITS: "@rightkit/release", ALLOW_HOST_EXCLUSION: "", ALLOW_NO_TEST: "", ALLOW_IGNORED: "", ALLOW_MISSING_ASSETS: "", ...extra };
  const [name] = String(env.UNITS).split(/\s+/);
  const version = { "@rightkit/release": "0.6.7", "@rightkit/ci": "0.3.2" }[name] ?? "0.6.7";
  return { QUALIFICATION: JSON.stringify(qual({ kind: env.REGISTRY === "npm" ? "npm" : "cargo", name, version })), ...env };
};
const hostedEnv = brokerEnv;
/** A committed tree with packages/release (@rightkit/release 0.6.7) and packages/ci (@rightkit/ci 0.3.2). */
const PACKAGES = { release: { name: "@rightkit/release", version: "0.6.7" }, ci: { name: "@rightkit/ci", version: "0.3.2" } };
const git = (packages = PACKAGES) => (...args) => {
  if (args[0] === "ls-tree") return `${Object.keys(packages).join("\n")}\n`;
  if (args[0] === "show") { const dir = /:packages\/([^/]+)\/package\.json$/.exec(args[1])?.[1]; if (packages[dir]) return JSON.stringify(packages[dir]); }
  throw new Error(`unexpected git ${args.join(" ")}`);
};
const greenGate = (sha = SHA) => () => ({ status: 0, stdout: `check: ${sha.slice(0, 12)} passed 58 rules (release tier) in 7.1s\nrightkit-gate: class A pass: 12 deterministic scan(s) over 900 file(s) of ${sha}\n` });
const GATE = { command: `rightkit-gate check --commit ${SHA} --rule-tier release`, exit: 0, rules: 58, classAScans: 12, classAFiles: 900 };
const npm = (versions, name) => async (url) => (versions === null ? { status: 404, ok: false } : { status: 200, ok: true, json: async () => ({ name: name ?? decodeURIComponent(url.split("/").pop()), versions: Object.fromEntries(versions.map((v) => [v, {}])) }) });

test("broker admission: one unpublished @rightkit/* package at the run's commit, with the gate's qualification receipt and exactly the committed policy, is admitted", async () => {
  const receipt = await admit({ env: brokerEnv(), git: git(), runGate: greenGate(), fetchImpl: npm(["0.6.6"]), policy: policy() });
  assert.deepEqual(receipt, { schema: ADMISSION_SCHEMA, repository: "bogusyogi/rightkit", sourceSha: SHA, runId: "77", runAttempt: "1", registry: "npm", units: ["@rightkit/release"], allow: { hostExclusion: [], noTest: [], ignored: [], missingAssets: [] }, mode: "broker", requestId: "req-1", versions: { "@rightkit/release": "0.6.7" }, gate: GATE, qualification: QUAL });
  // A package npm has never seen is admitted too (only a 404 counts as absent).
  assert.equal((await admit({ runGate: greenGate(), env: hostedEnv({ UNITS: "@rightkit/ci" }), git: git(), fetchImpl: npm(null), policy: policy() })).versions["@rightkit/ci"], "0.3.2");
  // The committed policy's allowsets are the only ones admitted, in any order.
  const granted = { ...NONE, allow_missing_assets: ["@rightkit/release"] };
  assert.deepEqual((await admit({ runGate: greenGate(), env: hostedEnv({ ALLOW_MISSING_ASSETS: "@rightkit/release" }), git: git(), fetchImpl: npm([]), policy: policy(granted) })).allow.missingAssets, ["@rightkit/release"]);
  // And the uploader accepts exactly that receipt against the sealed manifest and verify allowsets.
  const expected = { repository: "bogusyogi/rightkit", sourceSha: SHA, runId: "77", runAttempt: "1", registry: "npm", units: [{ name: "@rightkit/release", version: "0.6.7" }], allow: { hostExclusion: [], noTest: [], ignored: [], missingAssets: [] } };
  assert.equal(verifyAdmissionReceipt(receipt, expected), receipt);
  for (const suffix of ["", " --rule-tier compile"]) {
    assert.throws(() => verifyAdmissionReceipt({ ...receipt, gate: { ...receipt.gate, command: `rightkit-gate check --commit ${SHA}${suffix}` } }, expected), /no passed gate for this commit/);
  }
});

test("admission refuses wrong SHA, wrong scope, no request id, hosted self-admission, a missing or foreign qualification, a published version (replay) and allowsets the policy does not grant", async () => {
  const cases = [
    ["dispatched SHA is not the run's", hostedEnv({ EXPECTED_SHA: OTHER }), /not the run's commit/],
    ["outside Actions", hostedEnv({ GITHUB_ACTIONS: "" }), /only in Actions on main/],
    ["another branch", hostedEnv({ GITHUB_REF: "refs/heads/feature" }), /only in Actions on main/],
    ["no request id", hostedEnv({ REQUEST_ID: "" }), /needs the broker's request id/],
    ["hosted self-admission (retired)", hostedEnv({ ADMISSION: "hosted", REQUEST_ID: "" }), /hosted self-admission is retired/],
    ["an unknown mode", hostedEnv({ ADMISSION: "cloud" }), /admission must be broker/],
    ["no qualification receipt", hostedEnv({ QUALIFICATION: "" }), /needs the gate's qualification receipt/],
    ["a qualification for another commit", hostedEnv({ QUALIFICATION: JSON.stringify(qual({ commit: OTHER })) }), /qualification receipt is for bbbbbbbbbbbb/],
    ["a qualification for another unit", hostedEnv({ QUALIFICATION: JSON.stringify(qual({ name: "@rightkit/ci", version: "0.3.2" })) }), /covers npm:@rightkit\/ci; this run publishes npm @rightkit\/release/],
    ["a qualification with a failed stage", hostedEnv({ QUALIFICATION: JSON.stringify(qual({ stages: { check: { ok: true }, compile: { ok: true }, package: { ok: true }, qualify: { ok: false } } })) }), /qualify stage is not ok/],
    ["a qualification for another version", hostedEnv({ QUALIFICATION: JSON.stringify(qual({ version: "0.6.8" })) }), /qualified @rightkit\/release@0.6.8, the committed version is 0.6.7/],
    ["two packages", hostedEnv({ UNITS: "@rightkit/release @rightkit/ci" }), /this run publishes npm @rightkit\/release @rightkit\/ci/],
    ["a foreign scope", hostedEnv({ UNITS: "@other/release" }), /@rightkit\/\* packages/],
    ["not a workspace package", hostedEnv({ UNITS: "@rightkit/absent" }), /not exactly one workspace package/],
  ];
  for (const [name, env, pattern] of cases) await assert.rejects(admit({ env, git: git(), runGate: greenGate(), fetchImpl: npm([]), policy: policy() }), pattern, name);
  await assert.rejects(admit({ env: hostedEnv(), git: git(), runGate: greenGate(), fetchImpl: npm(["0.6.7"]), policy: policy() }), /already on npm; a version is published once/, "replay of a published version");
  await assert.rejects(admit({ env: hostedEnv(), git: git(), runGate: greenGate(), fetchImpl: async () => ({ status: 503, ok: false }), policy: policy() }), /npm registry returned 503/, "an unreadable registry is no proof of absence");
  await assert.rejects(admit({ runGate: greenGate(), env: hostedEnv({ ALLOW_IGNORED: "@rightkit/release" }), git: git(), fetchImpl: npm([]), policy: policy() }), /allow_ignored names @rightkit\/release, which the committed exception policy does not grant/, "a wider allowset");
  // A narrower allowset than the policy grants is admitted (it can only run more).
  assert.deepEqual((await admit({ env: hostedEnv(), git: git(), runGate: greenGate(), fetchImpl: npm([]), policy: policy({ ...NONE, allow_missing_assets: ["@rightkit/release"] }) })).allow.missingAssets, []);
  await assert.rejects(admit({ env: hostedEnv(), runGate: greenGate(), git: git({ release: { name: "@rightkit/release", version: "0.6.7", private: true } }), fetchImpl: npm([]), policy: policy() }), /is private/);
  await assert.rejects(admit({ env: hostedEnv(), runGate: greenGate(), git: git({ release: { name: "@rightkit/release" } }), fetchImpl: npm([]), policy: policy() }), /no publishable version/);
});

test("the registry answer fails closed: only a 404 is absence, and a 200 must be this package's own record", async () => {
  const answer = (body, status = 200) => async () => ({ status, ok: status >= 200 && status < 300, json: async () => { if (body instanceof Error) throw body; return body; } });
  const run = (fetchImpl) => admit({ env: hostedEnv(), git: git(), runGate: greenGate(), fetchImpl, policy: policy() });
  // Positives: a valid packument without this version, and a 404 (never published).
  assert.deepEqual((await run(answer({ name: "@rightkit/release", versions: { "0.6.6": {} } }))).versions, { "@rightkit/release": "0.6.7" });
  assert.deepEqual((await run(answer(null, 404))).versions, { "@rightkit/release": "0.6.7" });
  // A valid packument holding this version is a published version (replay).
  await assert.rejects(run(answer({ name: "@rightkit/release", versions: { "0.6.7": {} } })), /already on npm/);
  // Malformed or foreign 200 answers are unreadable, never absence.
  for (const [name, body] of [
    ["empty object", {}],
    ["no versions", { name: "@rightkit/release" }],
    ["versions not an object", { name: "@rightkit/release", versions: "0.6.7" }],
    ["versions an array", { name: "@rightkit/release", versions: ["0.6.6"] }],
    ["versions null", { name: "@rightkit/release", versions: null }],
    ["another package's packument", { name: "@rightkit/ci", versions: {} }],
    ["no name", { versions: {} }],
    ["a JSON array", []],
    ["JSON null", null],
    ["not JSON", new SyntaxError("Unexpected token")],
  ]) await assert.rejects(run(answer(body)), /malformed or foreign packument|no JSON record/, name);
  await assert.rejects(run(answer({}, 500)), /npm registry returned 500/);
  // crates.io: a 200 must be exactly this crate's version record.
  const crateEnv = { ...ACTIONS, ADMISSION: "broker", REQUEST_ID: "job-1", EXPECTED_SHA: SHA, REGISTRY: "crates", UNITS: "rightkit-qa", QUALIFICATION: JSON.stringify(qual({ kind: "cargo", name: "rightkit-qa", version: "0.9.2" })), ALLOW_HOST_EXCLUSION: "", ALLOW_NO_TEST: "", ALLOW_IGNORED: "", ALLOW_MISSING_ASSETS: "" };
  const crateGit = (...args) => (args[0] === "show" && args[1] === `${SHA}:crates/rightkit-qa/Cargo.toml` ? '[package]\nname = "rightkit-qa"\nversion = "0.9.2"\n' : git()(...args));
  const runCrate = (fetchImpl) => admit({ env: crateEnv, git: crateGit, runGate: greenGate(), fetchImpl, policy: policy() });
  assert.deepEqual((await runCrate(answer(null, 404))).versions, { "rightkit-qa": "0.9.2" });
  await assert.rejects(runCrate(answer({ version: { crate: "rightkit-qa", num: "0.9.2" } })), /already on crates/);
  for (const [name, body] of [["empty object", {}], ["another crate", { version: { crate: "rightkit-qb", num: "0.9.2" } }], ["another version", { version: { crate: "rightkit-qa", num: "0.9.1" } }], ["errors body", { errors: [{ detail: "Not Found" }] }]]) {
    await assert.rejects(runCrate(answer(body)), /record of another or no crate version/, name);
  }
});

test("broker mode grants nothing: an invented request id gets the same gate, version and policy checks as hosted", async () => {
  const crate = (versions) => async (url) => { assert.match(url, /^https:\/\/crates\.io\/api\/v1\/crates\/rightkit-qa\/0\.9\.2$/); return versions.includes("0.9.2") ? { status: 200, ok: true, json: async () => ({ version: { crate: "rightkit-qa", num: "0.9.2" } }) } : { status: 404, ok: false }; };
  const crates = (...args) => (args[0] === "show" && args[1] === `${SHA}:crates/rightkit-qa/Cargo.toml` ? '[package]\nname = "rightkit-qa"\nversion = "0.9.2"\n\n[dependencies]\nx = { version = "1" }\n' : git()(...args));
  const env = { ...ACTIONS, ADMISSION: "broker", REQUEST_ID: "invented-by-a-raw-dispatch", EXPECTED_SHA: SHA, REGISTRY: "crates", UNITS: "rightkit-qa", QUALIFICATION: JSON.stringify(qual({ kind: "cargo", name: "rightkit-qa", version: "0.9.2" })), ALLOW_HOST_EXCLUSION: "", ALLOW_NO_TEST: "", ALLOW_IGNORED: "", ALLOW_MISSING_ASSETS: "rightkit-qa" };
  const granted = () => ({ ...NONE, allow_missing_assets: ["rightkit-qa"] });
  // Original denied path, raw: a dispatch with admission=broker and an invented id. The id is recorded, never trusted:
  // a red gate refuses it,
  await assert.rejects(admit({ env, git: crates, runGate: () => ({ status: 1, stdout: "rightkit-gate: BLOCKED (1)\n" }), fetchImpl: crate([]), policy: granted }), /rightkit-gate check did not pass/);
  // a published version refuses it,
  await assert.rejects(admit({ env, git: crates, runGate: greenGate(), fetchImpl: crate(["0.9.2"]), policy: granted }), /already on crates/);
  // an allowset the policy does not grant refuses it,
  await assert.rejects(admit({ env: { ...env, ALLOW_IGNORED: "rightkit-qa" }, git: crates, runGate: greenGate(), fetchImpl: crate([]), policy: granted }), /allow_ignored names rightkit-qa/);
  // and a unit outside the workspace refuses it.
  await assert.rejects(admit({ env: { ...env, UNITS: "rightkit-absent", QUALIFICATION: JSON.stringify(qual({ kind: "cargo", name: "rightkit-absent", version: "0.9.2" })) }, git: crates, runGate: greenGate(), fetchImpl: crate([]), policy: granted }), /not a crate of this workspace/);
  // Only when every check passes is it admitted, with the version and gate bound like any other request.
  const receipt = await admit({ env, git: crates, runGate: greenGate(), fetchImpl: crate([]), policy: granted });
  assert.deepEqual([receipt.mode, receipt.requestId, receipt.versions, receipt.gate], ["broker", env.REQUEST_ID, { "rightkit-qa": "0.9.2" }, GATE]);
  await assert.rejects(admit({ env: { ...env, REQUEST_ID: "" }, git: crates, runGate: greenGate(), policy: granted }), /needs the broker's request id/);
  // npm under broker mode is still @rightkit/* only.
  await assert.rejects(admit({ env: { ...env, REGISTRY: "npm", UNITS: "left-pad", QUALIFICATION: JSON.stringify(qual({ name: "left-pad" })) }, git: git(), runGate: greenGate(), fetchImpl: npm([]), policy: policy() }), /@rightkit\/\* packages/);
});

test("the gate verdict needs exit 0, this commit's app-check pass line and the class A pass line", () => {
  assert.deepEqual(gateVerdict(greenGate()(), SHA), GATE);
  const good = greenGate()();
  for (const [name, result] of [
    ["red", { status: 1, stdout: "rightkit-gate: BLOCKED (1)\n" }],
    ["legacy untiered summary", { ...good, stdout: good.stdout.replace(" (release tier)", "") }],
    ["compile tier", { ...good, stdout: good.stdout.replace("release tier", "compile tier") }],
    ["selected rules", { ...good, stdout: good.stdout.replace("release tier", "selected") }],
    ["malformed summary", { ...good, stdout: good.stdout.replace("in 7.1s", "in unknown") }],
    ["zero rules", { ...good, stdout: good.stdout.replace("58 rules", "0 rules") }],
    ["zero scans", { ...good, stdout: good.stdout.replace("12 deterministic", "0 deterministic") }],
    ["partial view (--only exits 3)", { status: 3, stdout: `check: ${SHA.slice(0, 12)} passed 1 rules (release tier) in 0.1s\n` }],
    ["exit 0 but no class A line", { status: 0, stdout: `check: ${SHA.slice(0, 12)} passed 58 rules (release tier) in 0.1s\n` }],
    ["exit 0 for another commit", { status: 0, stdout: `check: ${OTHER.slice(0, 12)} passed 58 rules (release tier) in 0.1s\nrightkit-gate: class A pass: 1 deterministic scan(s) over 1 file(s) of ${OTHER}\n` }],
    ["class A of another commit", { status: 0, stdout: `check: ${SHA.slice(0, 12)} passed 58 rules (release tier) in 0.1s\nrightkit-gate: class A pass: 1 deterministic scan(s) over 1 file(s) of ${OTHER}\n` }],
    ["no result", undefined],
  ]) assert.throws(() => gateVerdict(result, SHA), /rightkit-gate check did not pass/, name);
});

// ---- the actual boundary: the real rightkit-gate on a committed tree --------------------------------------------------------
const GATE_BIN = path.join(path.dirname(fileURLToPath(import.meta.url)), "..", "..", "packages", "release", "bin", "rightkit-gate.mjs");
/** A committed repository holding one @rightkit/* package and `extra` files; returns { root, sha, git, runGate }. */
function committedTree(t, extra = {}) {
  const root = fs.mkdtempSync(path.join(process.env.RIGHTKIT_TEST_SCRATCH ?? os.tmpdir(), "rk-admission-"));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const files = { "packages/foo/package.json": JSON.stringify({ name: "@rightkit/foo", version: "1.0.0", license: "MIT" }), ...extra };
  for (const [file, text] of Object.entries(files)) { fs.mkdirSync(path.dirname(path.join(root, file)), { recursive: true }); fs.writeFileSync(path.join(root, file), text); }
  const run = (...args) => { const r = spawnSync("git", ["-c", "user.name=t", "-c", "user.email=t@t", ...args], { cwd: root, encoding: "utf8" }); assert.equal(r.status, 0, r.stderr); return r.stdout; };
  run("init", "-q"); run("add", "-A"); run("commit", "-q", "-m", "fixture");
  const sha = run("rev-parse", "HEAD").trim();
  const gitRead = (...args) => { const r = spawnSync("git", args, { cwd: root, encoding: "utf8" }); if (r.status !== 0) throw new Error(`git ${args[0]} failed`); return r.stdout; };
  const runGate = (commit) => spawnSync(process.execPath, [GATE_BIN, "check", "--repo", root, "--commit", commit, "--rule-tier", "release"], { cwd: root, encoding: "utf8", env: { ...process.env, GITHUB_ACTIONS: "", CI: "" } });
  return { root, sha, git: gitRead, runGate };
}
const boundaryEnv = (sha, extra = {}) => ({ ...ACTIONS, GITHUB_SHA: sha, EXPECTED_SHA: sha, ADMISSION: "broker", REQUEST_ID: "req-boundary", QUALIFICATION: JSON.stringify(qual({ name: "@rightkit/foo", version: "1.0.0", commit: sha })), REGISTRY: "npm", UNITS: "@rightkit/foo", ALLOW_HOST_EXCLUSION: "", ALLOW_NO_TEST: "", ALLOW_IGNORED: "", ALLOW_MISSING_ASSETS: "", ...extra });

test("through the real gate: a red class A tree (broken committed JSON, the original refusal) admits nothing, whatever the request id", async (t) => {
  const red = committedTree(t, { "packages/foo/broken.json": '{"name":' });
  for (const mode of [{}, { REQUEST_ID: "invented-by-a-raw-dispatch" }]) {
    await assert.rejects(admit({ env: boundaryEnv(red.sha, mode), git: red.git, runGate: red.runGate, fetchImpl: () => assert.fail("a red gate reads no registry"), policy: policy() }), /rightkit-gate check did not pass/, JSON.stringify(mode));
  }
  const verdict = red.runGate(red.sha);
  assert.notEqual(verdict.status, 0); assert.match(verdict.stdout + verdict.stderr, /json-parse/);
});

test("through the real gate: the same tree with valid JSON is admitted, gate bound to its commit", async (t) => {
  const green = committedTree(t, { "packages/foo/ok.json": '{"name":"ok"}' });
  const receipt = await admit({ env: boundaryEnv(green.sha), git: green.git, runGate: green.runGate, fetchImpl: npm(null), policy: policy() });
  assert.equal(receipt.gate.command, `rightkit-gate check --commit ${green.sha} --rule-tier release`);
  assert.equal(receipt.gate.exit, 0); assert.ok(receipt.gate.rules > 0 && receipt.gate.classAScans > 0);
  assert.deepEqual(receipt.versions, { "@rightkit/foo": "1.0.0" });
});

test("the receipt artifact is per attempt", () => {
  assert.equal(admissionArtifactName("1"), "publish-admission-1");
  assert.throws(() => admissionArtifactName("0"), /invalid run attempt/);
});

// ---- the cloud entry ---------------------------------------------------------------------------------------------------------
/** A spawn double: git and gh answers by command, the gate's exit code, and every gh write recorded. */
function host({ status = "", head = SHA, main = SHA, gate = 0, gateOut, packages = PACKAGES } = {}) {
  const calls = [];
  const run = (cmd, args) => {
    calls.push([cmd, ...args]);
    const ok = (stdout) => ({ status: 0, stdout });
    if (cmd === "git" && args[0] === "status") return ok(status);
    if (cmd === "git" && args[0] === "rev-parse") return ok(`${head}\n`);
    if (cmd === "git" && args[0] === "ls-tree") return ok(`${Object.keys(packages).join("\n")}\n`);
    if (cmd === "git" && args[0] === "show") { const dir = /:packages\/([^/]+)\/package\.json$/.exec(args[1])?.[1]; return packages[dir] ? ok(JSON.stringify(packages[dir])) : { status: 128, stdout: "" }; }
    if (cmd === "gh" && args[1] === `repos/bogusyogi/rightkit/commits/main`) return ok(`${main}\n`);
    if (cmd === "gh" && args.includes("POST")) return ok("");
    if (cmd === process.execPath && args[0].endsWith("rightkit-gate.mjs")) return { status: gate, stdout: gateOut ?? (gate === 0 ? greenGate(head)().stdout : "rightkit-gate: BLOCKED (1)\n") };
    throw new Error(`unexpected ${cmd} ${args.join(" ")}`);
  };
  const dispatched = () => calls.filter((c) => c[0] === "gh" && c.includes("POST"));
  return { run, calls, dispatched };
}

test("the cloud entry is retired: dispatchHosted refuses before any gate run or gh call, whatever the tree looks like", () => {
  for (const options of [{}, { gate: 1 }, { main: OTHER }]) {
    const h = host(options);
    assert.throws(() => dispatchHosted({ unit: "@rightkit/release", root: "/repo", run: h.run, out: () => {}, policy: policy() }), /hosted dispatch is retired: publish through rightkit-gate --to release/);
    assert.deepEqual(h.calls, [], "nothing ran");
    assert.equal(h.dispatched().length, 0);
  }
});

test("bind: the sealed archive must carry the locally qualified sha256; a mismatch stops with both digests and the hosted listing", () => {
  const qualification = readQualificationInput(JSON.stringify(qual()), { sha: SHA, registry: "npm", units: ["@rightkit/release"] });
  const manifest = { schema: 1, registry: "npm", commit: SHA, units: [{ name: "@rightkit/release", version: "0.6.7", file: "rightkit-release-0.6.7.tgz", sha256: "d".repeat(64) }] };
  assert.deepEqual(bindPackedToQualification({ manifest, qualification }), { unit: "@rightkit/release", version: "0.6.7", sha256: "d".repeat(64), file: "rightkit-release-0.6.7.tgz" });
  const other = { ...manifest, units: [{ ...manifest.units[0], sha256: "e".repeat(64) }] };
  assert.throws(() => bindPackedToQualification({ manifest: other, qualification, listing: (entry) => `listing of ${entry.file}` }), (e) => /hosted sha256 e{64}, qualified d{64}/.test(e.message) && /no repack/.test(e.message) && /listing of rightkit-release-0.6.7.tgz/.test(e.message));
  assert.throws(() => bindPackedToQualification({ manifest: { ...manifest, commit: OTHER }, qualification }), /sealed manifest is for bbbbbbbbbbbb/);
  assert.throws(() => bindPackedToQualification({ manifest: { ...manifest, units: [] }, qualification }), /no entry for @rightkit\/release/);
  assert.throws(() => bindPackedToQualification({ manifest: { ...manifest, units: [{ ...manifest.units[0], version: "0.6.8" }] }, qualification }), /is 0.6.8, the qualification is 0.6.7/);
  assert.throws(() => readQualificationInput("not json", { sha: SHA, registry: "npm", units: ["@rightkit/release"] }), /not JSON/);
  assert.throws(() => readQualificationInput(JSON.stringify({ ...qual(), artifactSha256: "x" }), { sha: SHA, registry: "npm", units: ["@rightkit/release"] }), /names no artifact sha256/);
});

test("the original denied path stays denied: rightkit-dispatch without a broker job id refuses before any gh call", async () => {
  const errors = [];
  const code = await rightkitDispatch(["--workflow", "publish", "--registry", "npm", "--unit", "@rightkit/release", "--sha", SHA, "--qualification", "/nope/qualification.json"], { env: {}, gh: () => assert.fail("no gh call without a broker job"), stderr: (l) => errors.push(l), stdout: () => {} });
  assert.equal(code, 2);
  assert.match(errors.join("\n"), /no RIGHTKIT_JOB_ID; this runs only as the broker's rightkit-dispatch adapter/);
});
