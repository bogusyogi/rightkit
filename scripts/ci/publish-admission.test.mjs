import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { ADMISSION_SCHEMA, admissionArtifactName, admit, dispatchHosted, gateVerdict, verifyAdmissionReceipt } from "./publish-admission.mjs";
import { main as rightkitDispatch } from "./rightkit-dispatch.mjs";

const SHA = "a".repeat(40);
const OTHER = "b".repeat(40);
const NONE = { allow_host_exclusion: [], allow_no_test: [], allow_ignored: [], allow_missing_assets: [] };
const policy = (granted = NONE) => () => granted;
const ACTIONS = { GITHUB_ACTIONS: "true", GITHUB_REPOSITORY: "bogusyogi/rightkit", GITHUB_REF: "refs/heads/main", GITHUB_SHA: SHA, GITHUB_RUN_ID: "77", GITHUB_RUN_ATTEMPT: "1" };
const hostedEnv = (extra = {}) => ({ ...ACTIONS, ADMISSION: "hosted", REQUEST_ID: "", EXPECTED_SHA: SHA, REGISTRY: "npm", UNITS: "@rightkit/release", ALLOW_HOST_EXCLUSION: "", ALLOW_NO_TEST: "", ALLOW_IGNORED: "", ALLOW_MISSING_ASSETS: "", ...extra });
/** A committed tree with packages/release (@rightkit/release 0.6.7) and packages/ci (@rightkit/ci 0.3.2). */
const PACKAGES = { release: { name: "@rightkit/release", version: "0.6.7" }, ci: { name: "@rightkit/ci", version: "0.3.2" } };
const git = (packages = PACKAGES) => (...args) => {
  if (args[0] === "ls-tree") return `${Object.keys(packages).join("\n")}\n`;
  if (args[0] === "show") { const dir = /:packages\/([^/]+)\/package\.json$/.exec(args[1])?.[1]; if (packages[dir]) return JSON.stringify(packages[dir]); }
  throw new Error(`unexpected git ${args.join(" ")}`);
};
const greenGate = (sha = SHA) => () => ({ status: 0, stdout: `check: ${sha.slice(0, 12)} passed 58 rules in 7.1s\nrightkit-gate: class A pass: 12 deterministic scan(s) over 900 file(s) of ${sha}\n` });
const GATE = { command: `rightkit-gate check --commit ${SHA}`, exit: 0, rules: 58, classAScans: 12, classAFiles: 900 };
const npm = (versions, name) => async (url) => (versions === null ? { status: 404, ok: false } : { status: 200, ok: true, json: async () => ({ name: name ?? decodeURIComponent(url.split("/").pop()), versions: Object.fromEntries(versions.map((v) => [v, {}])) }) });

test("hosted admission: one unpublished @rightkit/* package at the run's commit, with exactly the committed policy, is admitted", async () => {
  const receipt = await admit({ env: hostedEnv(), git: git(), runGate: greenGate(), fetchImpl: npm(["0.6.6"]), policy: policy() });
  assert.deepEqual(receipt, { schema: ADMISSION_SCHEMA, repository: "bogusyogi/rightkit", sourceSha: SHA, runId: "77", runAttempt: "1", registry: "npm", units: ["@rightkit/release"], allow: { hostExclusion: [], noTest: [], ignored: [], missingAssets: [] }, mode: "hosted", requestId: "", versions: { "@rightkit/release": "0.6.7" }, gate: GATE });
  // A package npm has never seen is admitted too (only a 404 counts as absent).
  assert.equal((await admit({ runGate: greenGate(), env: hostedEnv({ UNITS: "@rightkit/ci" }), git: git(), fetchImpl: npm(null), policy: policy() })).versions["@rightkit/ci"], "0.3.2");
  // The committed policy's allowsets are the only ones admitted, in any order.
  const granted = { ...NONE, allow_missing_assets: ["@rightkit/release"] };
  assert.deepEqual((await admit({ runGate: greenGate(), env: hostedEnv({ ALLOW_MISSING_ASSETS: "@rightkit/release" }), git: git(), fetchImpl: npm([]), policy: policy(granted) })).allow.missingAssets, ["@rightkit/release"]);
  // And the uploader accepts exactly that receipt against the sealed manifest and verify allowsets.
  assert.equal(verifyAdmissionReceipt(receipt, { repository: "bogusyogi/rightkit", sourceSha: SHA, runId: "77", runAttempt: "1", registry: "npm", units: [{ name: "@rightkit/release", version: "0.6.7" }], allow: { hostExclusion: [], noTest: [], ignored: [], missingAssets: [] } }), receipt);
});

test("hosted admission refuses wrong SHA, wrong scope, a request id, a published version (replay) and allowsets the policy does not grant", async () => {
  const cases = [
    ["dispatched SHA is not the run's", hostedEnv({ EXPECTED_SHA: OTHER }), /not the run's commit/],
    ["outside Actions", hostedEnv({ GITHUB_ACTIONS: "" }), /only in Actions on main/],
    ["another branch", hostedEnv({ GITHUB_REF: "refs/heads/feature" }), /only in Actions on main/],
    ["a chosen request id", hostedEnv({ REQUEST_ID: "cloud-2026-10-26-x" }), /takes no request id/],
    ["crates", hostedEnv({ REGISTRY: "crates", UNITS: "rightkit-qa" }), /npm packages only/],
    ["two packages", hostedEnv({ UNITS: "@rightkit/release @rightkit/ci" }), /exactly one @rightkit/],
    ["a foreign scope", hostedEnv({ UNITS: "@other/release" }), /exactly one @rightkit/],
    ["not a workspace package", hostedEnv({ UNITS: "@rightkit/absent" }), /not exactly one workspace package/],
    ["an unknown mode", hostedEnv({ ADMISSION: "cloud" }), /broker or hosted/],
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
  const crateEnv = { ...ACTIONS, ADMISSION: "broker", REQUEST_ID: "job-1", EXPECTED_SHA: SHA, REGISTRY: "crates", UNITS: "rightkit-qa", ALLOW_HOST_EXCLUSION: "", ALLOW_NO_TEST: "", ALLOW_IGNORED: "", ALLOW_MISSING_ASSETS: "" };
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
  const env = { ...ACTIONS, ADMISSION: "broker", REQUEST_ID: "invented-by-a-raw-dispatch", EXPECTED_SHA: SHA, REGISTRY: "crates", UNITS: "rightkit-qa", ALLOW_HOST_EXCLUSION: "", ALLOW_NO_TEST: "", ALLOW_IGNORED: "", ALLOW_MISSING_ASSETS: "rightkit-qa" };
  const granted = () => ({ ...NONE, allow_missing_assets: ["rightkit-qa"] });
  // Original denied path, raw: a dispatch with admission=broker and an invented id. The id is recorded, never trusted:
  // a red gate refuses it,
  await assert.rejects(admit({ env, git: crates, runGate: () => ({ status: 1, stdout: "rightkit-gate: BLOCKED (1)\n" }), fetchImpl: crate([]), policy: granted }), /rightkit-gate check did not pass/);
  // a published version refuses it,
  await assert.rejects(admit({ env, git: crates, runGate: greenGate(), fetchImpl: crate(["0.9.2"]), policy: granted }), /already on crates/);
  // an allowset the policy does not grant refuses it,
  await assert.rejects(admit({ env: { ...env, ALLOW_IGNORED: "rightkit-qa" }, git: crates, runGate: greenGate(), fetchImpl: crate([]), policy: granted }), /allow_ignored names rightkit-qa/);
  // and a unit outside the workspace refuses it.
  await assert.rejects(admit({ env: { ...env, UNITS: "rightkit-absent" }, git: crates, runGate: greenGate(), fetchImpl: crate([]), policy: granted }), /not a crate of this workspace/);
  // Only when every check passes is it admitted, with the version and gate bound like any other request.
  const receipt = await admit({ env, git: crates, runGate: greenGate(), fetchImpl: crate([]), policy: granted });
  assert.deepEqual([receipt.mode, receipt.requestId, receipt.versions, receipt.gate], ["broker", env.REQUEST_ID, { "rightkit-qa": "0.9.2" }, GATE]);
  await assert.rejects(admit({ env: { ...env, REQUEST_ID: "" }, git: crates, runGate: greenGate(), policy: granted }), /needs the broker's request id/);
  // npm under broker mode is still @rightkit/* only.
  await assert.rejects(admit({ env: { ...env, REGISTRY: "npm", UNITS: "left-pad" }, git: git(), runGate: greenGate(), fetchImpl: npm([]), policy: policy() }), /@rightkit\/\* packages/);
});

test("the gate verdict needs exit 0, this commit's app-check pass line and the class A pass line", () => {
  assert.deepEqual(gateVerdict(greenGate()(), SHA), GATE);
  for (const [name, result] of [
    ["red", { status: 1, stdout: "rightkit-gate: BLOCKED (1)\n" }],
    ["partial view (--only exits 3)", { status: 3, stdout: `check: ${SHA.slice(0, 12)} passed 1 rules in 0.1s\n` }],
    ["exit 0 but no class A line", { status: 0, stdout: `check: ${SHA.slice(0, 12)} passed 58 rules in 0.1s\n` }],
    ["exit 0 for another commit", { status: 0, stdout: `check: ${OTHER.slice(0, 12)} passed 58 rules in 0.1s\nrightkit-gate: class A pass: 1 deterministic scan(s) over 1 file(s) of ${OTHER}\n` }],
    ["class A of another commit", { status: 0, stdout: `check: ${SHA.slice(0, 12)} passed 58 rules in 0.1s\nrightkit-gate: class A pass: 1 deterministic scan(s) over 1 file(s) of ${OTHER}\n` }],
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
  const runGate = (commit) => spawnSync(process.execPath, [GATE_BIN, "check", "--repo", root, "--commit", commit], { cwd: root, encoding: "utf8", env: { ...process.env, GITHUB_ACTIONS: "", CI: "" } });
  return { root, sha, git: gitRead, runGate };
}
const boundaryEnv = (sha, extra = {}) => ({ ...ACTIONS, GITHUB_SHA: sha, EXPECTED_SHA: sha, ADMISSION: "hosted", REQUEST_ID: "", REGISTRY: "npm", UNITS: "@rightkit/foo", ALLOW_HOST_EXCLUSION: "", ALLOW_NO_TEST: "", ALLOW_IGNORED: "", ALLOW_MISSING_ASSETS: "", ...extra });

test("through the real gate: a red class A tree (broken committed JSON, the original refusal) admits nothing in either mode", async (t) => {
  const red = committedTree(t, { "packages/foo/broken.json": '{"name":' });
  for (const mode of [{}, { ADMISSION: "broker", REQUEST_ID: "invented-by-a-raw-dispatch" }]) {
    await assert.rejects(admit({ env: boundaryEnv(red.sha, mode), git: red.git, runGate: red.runGate, fetchImpl: () => assert.fail("a red gate reads no registry"), policy: policy() }), /rightkit-gate check did not pass/, JSON.stringify(mode));
  }
  const verdict = red.runGate(red.sha);
  assert.notEqual(verdict.status, 0); assert.match(verdict.stdout + verdict.stderr, /json-parse/);
});

test("through the real gate: the same tree with valid JSON is admitted, gate bound to its commit", async (t) => {
  const green = committedTree(t, { "packages/foo/ok.json": '{"name":"ok"}' });
  const receipt = await admit({ env: boundaryEnv(green.sha), git: green.git, runGate: green.runGate, fetchImpl: npm(null), policy: policy() });
  assert.equal(receipt.gate.command, `rightkit-gate check --commit ${green.sha}`);
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

test("cloud entry: a green rightkit-gate check at origin main dispatches admission=hosted with no request id and the committed allowsets", () => {
  const h = host();
  const lines = [];
  const granted = { ...NONE, allow_missing_assets: ["@rightkit/release"] };
  assert.deepEqual(dispatchHosted({ unit: "@rightkit/release", root: "/repo", run: h.run, out: (l) => lines.push(l), policy: () => granted }), { sha: SHA, unit: "@rightkit/release", version: "0.6.7" });
  const gate = h.calls.findIndex((c) => c[0] === process.execPath);
  assert.deepEqual(h.calls[gate].slice(1), ["/repo/packages/release/bin/rightkit-gate.mjs", "check", "--repo", "/repo", "--commit", SHA], "the same gate, the full check, on the exact commit");
  assert.ok(gate < h.calls.findIndex((c) => c.includes("POST")), "the gate runs before the dispatch");
  const [post] = h.dispatched();
  assert.equal(h.dispatched().length, 1);
  assert.deepEqual(post.slice(post.indexOf("-f")), ["-f", "ref=main", "-f", "inputs[admission]=hosted", "-f", "inputs[request_id]=", "-f", `inputs[expected_sha]=${SHA}`, "-f", "inputs[registry]=npm", "-f", "inputs[units]=@rightkit/release", "-f", "inputs[allow_host_exclusion]=", "-f", "inputs[allow_no_test]=", "-f", "inputs[allow_ignored]=", "-f", "inputs[allow_missing_assets]=@rightkit/release"]);
  assert.match(lines.at(-1), /admission=hosted for @rightkit\/release@0\.6\.7/);
});

test("cloud entry dispatches nothing for a red gate, a moved main, a dirty tree or a wrong scope", () => {
  for (const [name, options, unit, pattern] of [
    ["red gate", { gate: 1 }, "@rightkit/release", /rightkit-gate check did not pass/],
    ["partial gate view is not green", { gate: 3 }, "@rightkit/release", /rightkit-gate check did not pass/],
    ["exit 0 without the pass lines", { gate: 0, gateOut: "" }, "@rightkit/release", /rightkit-gate check did not pass/],
    ["HEAD is not origin main", { main: OTHER }, "@rightkit/release", /is not origin main/],
    ["uncommitted tracked changes", { status: " M package.json" }, "@rightkit/release", /uncommitted tracked changes/],
    ["not a workspace package", {}, "@rightkit/absent", /not exactly one workspace package/],
    ["a crate", {}, "rightkit-qa", /exactly one @rightkit/],
    ["two packages", {}, "@rightkit/release @rightkit/ci", /exactly one @rightkit/],
  ]) {
    const h = host(options);
    assert.throws(() => dispatchHosted({ unit, root: "/repo", run: h.run, out: () => {}, policy: policy() }), pattern, name);
    assert.equal(h.dispatched().length, 0, name);
  }
});

test("the original denied path stays denied: rightkit-dispatch without a broker job id refuses before any gh call", async () => {
  const errors = [];
  const code = await rightkitDispatch(["--workflow", "publish", "--registry", "npm", "--unit", "@rightkit/release", "--sha", SHA], { env: {}, gh: () => assert.fail("no gh call without a broker job"), stderr: (l) => errors.push(l), stdout: () => {} });
  assert.equal(code, 2);
  assert.match(errors.join("\n"), /no RIGHTKIT_JOB_ID; this runs only as the broker's rightkit-dispatch adapter/);
});
