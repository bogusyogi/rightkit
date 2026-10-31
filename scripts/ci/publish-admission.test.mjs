import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";
import { resolveManagedRoots } from "../../packages/release/managed-roots.mjs";
import { npmContentDigest, SWAP_HELPER_ARCHIVE_BIN, SWAP_HELPER_ARCHIVE_RECEIPT, exclusionsFor } from "../../packages/release/pack-digest.mjs";
import { carriedCallNonce, qualificationNonce } from "../../packages/release/provenance-verify.mjs";
import { expectedStageCommands } from "../../packages/release/stage-commands.mjs";
import { makeSigner } from "../../packages/release/test-support/provenance-signer.mjs";
import { tgzOf } from "../../packages/release/test-support/tgz.mjs";
import { ADMISSION_SCHEMA, QUALIFICATION_SCHEMA, admissionArtifactName, admit, bindPackedToQualification, dispatchHosted, gateVerdict, readQualificationInput, verifyAdmissionReceipt, verifyQualificationReceipts } from "./publish-admission.mjs";
import { main as rightkitDispatch } from "./rightkit-dispatch.mjs";

const SHA = "a".repeat(40);
const OTHER = "b".repeat(40);
const NONE = { allow_host_exclusion: [], allow_no_test: [], allow_ignored: [], allow_missing_assets: [] };
const policy = (granted = NONE) => () => granted;
const ACTIONS = { GITHUB_ACTIONS: "true", GITHUB_REPOSITORY: "bogusyogi/rightkit", GITHUB_REF: "refs/heads/main", GITHUB_SHA: SHA, GITHUB_RUN_ID: "77", GITHUB_RUN_ATTEMPT: "1" };
/** The enrolled host: a key pair standing in for the Mac broker's, whose pins are committed in ci/rk-pipeline.json at the dispatched commit. */
const SIGNER = makeSigner();
const TREE = "c".repeat(40);
const PIPELINE = JSON.stringify({ attestation: { darwin: SIGNER.pins } });
/** What each stage's signed requests must show: the stage builders' own commands for the unit (the same derivation the admission uses). */
const stageRows = ({ kind, name, stage }) => expectedStageCommands({ kind, unit: name, config: {}, manifest: { scripts: { test: "x" } }, platform: "darwin" })[stage];
const argvOf = (row) => row.argvs[0].map((a) => (typeof a === "string" ? a : "/tmp/scratch"));
/** A carried call's signed workspace is the directory of the checkout it ran in (`row.cwd`), a pnpm job's the checkout itself. */
const dirOf = (row, workspace) => (row.cwd && row.cwd !== "." ? `${workspace.replace(/\/+$/, "")}/${row.cwd}` : workspace);
const requestOf = (row, workspace) => ({ kind: row.kind, ...(row.adapter ? { adapter: row.adapter } : {}), shim: row.kind, requestedArgv: argvOf(row), workspace: dirOf(row, workspace), workspaceRoot: dirOf(row, workspace) });
/** The signed snapshotEvidence a carried or adapter job carries: the dispatched commit and tree, the snapshot at that tree throughout, one handle. */
const evidenceOf = ({ jobId, commit, tree, snapshot = "/snap/rk", handle = "own-1" }) => ({ requestId: jobId, commit: { sha: commit, tree }, snapshot: { path: snapshot, treeAtStart: tree, treeAtExit: tree }, ownership: { handle } });
/** The broker's signed receipts for one stage of one unit, as the gate embeds them: one job per command of the stage, nonce bound to
 * commit, tree, unit and stage (a carried cargo call or pnpm adapter job: derived for its directory and argv), snapshot evidence of the dispatched commit, run in a
 * checkout. `jobs` overrides the ids (each runs the stage's key command); `evidence(base, i)` returns the job's snapshotEvidence (undefined: none);
 * `nonceOf({ row, request, stageNonce, i })` overrides the nonce. */
const stageReceipts = ({ commit = SHA, tree = TREE, kind, name, stage, signer = SIGNER, jobs, workspace = "/work/rightkit", rows, request, requestFor, evidence = (base) => base, nonceOf, payload: extra = {}, ...over }) => {
  const all = rows ?? stageRows({ kind, name, stage });
  const ids = jobs ?? all.map((_, i) => `${stage}-job-${i + 1}`);
  const stageNonce = qualificationNonce({ commit, tree, unit: `${kind}:${name}`, stage });
  return ids.map((jobId, i) => {
    const row = all[i] ?? all[0];
    const req = request ?? (requestFor ?? requestOf)(row, workspace, i);
    const nonce = nonceOf ? nonceOf({ row, request: req, stageNonce, i }) : row.kind === req.kind && (row.kind === "cargo" || row.kind === "adapter") ? carriedCallNonce({ stageNonce, cwd: row.cwd ?? ".", argv: req.requestedArgv }) : stageNonce;
    const snapshotEvidence = evidence(evidenceOf({ jobId, commit, tree }), i);
    return signer.receipt({ jobId, nonce, request: req, payload: { ...(snapshotEvidence === undefined ? {} : { snapshotEvidence }), ...extra }, ...over });
  });
};
const CONTENT = "f".repeat(64);
const HELPER_PATHS = [SWAP_HELPER_ARCHIVE_BIN, SWAP_HELPER_ARCHIVE_RECEIPT];
/** rightkit-gate's qualification receipt as the dispatcher forwards it (one unit, every local stage ok, signed broker receipts for compile and package, one artifact sha). */
const qual = ({ kind = "npm", name = "@rightkit/release", version = "0.6.7", commit = SHA, tree = TREE, receiptFor = {}, ...over } = {}) => ({ schema: QUALIFICATION_SCHEMA, commit, tree, runtime: "r".repeat(64), executor: "broker",
  unit: { kind, name, version }, stages: { check: { ok: true }, compile: { ok: true, receipts: stageReceipts({ commit, tree, kind, name, stage: "compile", ...receiptFor }) }, package: { ok: true, receipts: stageReceipts({ commit, tree, kind, name, stage: "package", ...receiptFor }) }, qualify: { ok: true } }, artifactSha256: "d".repeat(64), ...(kind === "npm" ? { contentSha256: CONTENT, ...(name === "@rightkit/release" ? { contentExcluded: HELPER_PATHS } : {}) } : {}), ...over });
const provenanceOf = ({ kind, name }) => Object.fromEntries(["compile", "package"].map((stage) => [stage, stageRows({ kind, name, stage }).map((row, i) => ({ jobId: `${stage}-job-${i + 1}`, host: SIGNER.pins.host, keyId: SIGNER.pins.keyId, requestDigest: "7".repeat(64), requestedArgv: argvOf(row) }))]));
const QUAL = { schema: QUALIFICATION_SCHEMA, commit: SHA, tree: TREE, runtime: "r".repeat(64), executor: "broker", unit: { kind: "npm", name: "@rightkit/release", version: "0.6.7" }, artifactSha256: "d".repeat(64), contentSha256: CONTENT, contentExcluded: [...HELPER_PATHS].sort(),
  provenance: provenanceOf({ kind: "npm", name: "@rightkit/release" }), unattested: ["check", "qualify"] };
/** The only admitted shape: the broker's dispatch of rightkit-gate --to release, carrying the qualification for the first unit named. */
const brokerEnv = (extra = {}) => {
  const env = { ...ACTIONS, ADMISSION: "broker", REQUEST_ID: "req-1", EXPECTED_SHA: SHA, REGISTRY: "npm", UNITS: "@rightkit/release", ALLOW_HOST_EXCLUSION: "", ALLOW_NO_TEST: "", ALLOW_IGNORED: "", ALLOW_MISSING_ASSETS: "", ...extra };
  const [name] = String(env.UNITS).split(/\s+/);
  const version = { "@rightkit/release": "0.6.7", "@rightkit/ci": "0.3.2" }[name] ?? "0.6.7";
  return { QUALIFICATION: JSON.stringify(qual({ kind: env.REGISTRY === "npm" ? "npm" : "cargo", name, version })), ...env };
};
const hostedEnv = brokerEnv;
/** A committed tree with packages/release (@rightkit/release 0.6.7) and packages/ci (@rightkit/ci 0.3.2). */
const PACKAGES = { release: { name: "@rightkit/release", version: "0.6.7", scripts: { test: "x" } }, ci: { name: "@rightkit/ci", version: "0.3.2", scripts: { test: "x" } } };
const git = (packages = PACKAGES) => (...args) => {
  if (args[0] === "rev-parse" && args[1] === `${SHA}^{tree}`) return `${TREE}\n`;
  if (args[0] === "show" && args[1] === `${SHA}:ci/rk-pipeline.json`) return PIPELINE;
  if (args[0] === "show" && args[1] === `${SHA}:ci/gate.config.json`) return "{}";
  if (args[0] === "ls-tree") return `${Object.keys(packages).join("\n")}\n`;
  if (args[0] === "show") { const dir = /:packages\/([^/]+)\/package\.json$/.exec(args[1])?.[1]; if (packages[dir]) return JSON.stringify(packages[dir]); }
  throw new Error(`unexpected git ${args.join(" ")}`);
};
const greenGate = (sha = SHA) => () => ({ status: 0, stdout: `check: ${sha.slice(0, 12)} passed 58 rules (release tier) in 7.1s\nrightkit-gate: class A content pass: 12 deterministic scan(s) over 900 file(s) of ${sha}\n` });
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
    ["exit 0 for another commit", { status: 0, stdout: `check: ${OTHER.slice(0, 12)} passed 58 rules (release tier) in 0.1s\nrightkit-gate: class A content pass: 1 deterministic scan(s) over 1 file(s) of ${OTHER}\n` }],
    ["class A of another commit", { status: 0, stdout: `check: ${SHA.slice(0, 12)} passed 58 rules (release tier) in 0.1s\nrightkit-gate: class A content pass: 1 deterministic scan(s) over 1 file(s) of ${OTHER}\n` }],
    ["no result", undefined],
  ]) assert.throws(() => gateVerdict(result, SHA), /rightkit-gate check did not pass/, name);
});

// ---- the actual boundary: the real rightkit-gate on a committed tree --------------------------------------------------------
const GATE_BIN = path.join(path.dirname(fileURLToPath(import.meta.url)), "..", "..", "packages", "release", "bin", "rightkit-gate.mjs");
// The gate's snapshot route (every host but hosted CI) keeps its snapshots under the host's managed root. The gate never creates that root, and on
// Mac and Windows no environment variable moves it (snapshotRoot resolves it with env: {}), so a hosted Mac or Windows runner, which has none, cannot
// take the snapshot route at all. There the gate runs the route production runs in this same job (publish.yml's scans job runs this `check` with
// GITHUB_ACTIONS=true: the checkout is the commit, content-checked in place), so GITHUB_ACTIONS and CI stay as the runner set them. Every other
// host (Linux, a managed Mac or Dell, where the root exists) keeps the snapshot route, with both cleared as before.
const HOSTED_WITHOUT_MANAGED_ROOT = process.env.GITHUB_ACTIONS === "true" && (process.platform === "darwin" || process.platform === "win32")
  && !fs.existsSync(resolveManagedRoots({ env: {}, platform: process.platform }).dataRoot);
const gateEnv = () => (HOSTED_WITHOUT_MANAGED_ROOT ? { ...process.env } : { ...process.env, GITHUB_ACTIONS: "", CI: "" });
/** A committed repository holding one @rightkit/* package and `extra` files; returns { root, sha, git, runGate }. */
function committedTree(t, extra = {}) {
  const root = fs.mkdtempSync(path.join(process.env.RIGHTKIT_TEST_SCRATCH ?? os.tmpdir(), "rk-admission-"));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const files = { "packages/foo/package.json": JSON.stringify({ name: "@rightkit/foo", version: "1.0.0", license: "MIT", scripts: { test: "x" } }), ...extra };
  for (const [file, text] of Object.entries(files)) { fs.mkdirSync(path.dirname(path.join(root, file)), { recursive: true }); fs.writeFileSync(path.join(root, file), text); }
  const run = (...args) => { const r = spawnSync("git", ["-c", "user.name=t", "-c", "user.email=t@t", ...args], { cwd: root, encoding: "utf8" }); assert.equal(r.status, 0, r.stderr); return r.stdout; };
  run("init", "-q"); run("add", "-A"); run("commit", "-q", "-m", "fixture");
  const sha = run("rev-parse", "HEAD").trim();
  const tree = run("rev-parse", "HEAD^{tree}").trim();
  const realGit = (...args) => { const r = spawnSync("git", args, { cwd: root, encoding: "utf8" }); if (r.status !== 0) throw new Error(`git ${args[0]} failed`); return r.stdout; };
  // The enrolled pins are committed at the dispatched commit in the real repository; this fixture keeps ci/ out of the tree the real gate
  // scans and serves the one file from the test's key, everything else (including the tree id) from real git.
  const gitRead = (...args) => (args[0] === "show" && args[1] === `${sha}:ci/rk-pipeline.json` ? PIPELINE : args[0] === "show" && args[1] === `${sha}:ci/gate.config.json` ? "{}" : realGit(...args));
  const runGate = (commit) => spawnSync(process.execPath, [GATE_BIN, "check", "--repo", root, "--commit", commit, "--rule-tier", "release"], { cwd: root, encoding: "utf8", env: gateEnv() });
  return { root, sha, tree, git: gitRead, runGate };
}
const boundaryEnv = (sha, tree, extra = {}) => ({ ...ACTIONS, GITHUB_SHA: sha, EXPECTED_SHA: sha, ADMISSION: "broker", REQUEST_ID: "req-boundary", QUALIFICATION: JSON.stringify(qual({ name: "@rightkit/foo", version: "1.0.0", commit: sha, tree })), REGISTRY: "npm", UNITS: "@rightkit/foo", ALLOW_HOST_EXCLUSION: "", ALLOW_NO_TEST: "", ALLOW_IGNORED: "", ALLOW_MISSING_ASSETS: "", ...extra });

test("through the real gate: a red class A tree (broken committed JSON, the original refusal) admits nothing, whatever the request id", async (t) => {
  const red = committedTree(t, { "packages/foo/broken.json": '{"name":' });
  for (const mode of [{}, { REQUEST_ID: "invented-by-a-raw-dispatch" }]) {
    await assert.rejects(admit({ env: boundaryEnv(red.sha, red.tree, mode), git: red.git, runGate: red.runGate, fetchImpl: () => assert.fail("a red gate reads no registry"), policy: policy() }), /rightkit-gate check did not pass/, JSON.stringify(mode));
  }
  const verdict = red.runGate(red.sha);
  assert.notEqual(verdict.status, 0); assert.match(verdict.stdout + verdict.stderr, /json-parse/);
});

test("through the real gate: the same tree with valid JSON is admitted, gate bound to its commit", async (t) => {
  const green = committedTree(t, { "packages/foo/ok.json": '{"name":"ok"}' });
  const receipt = await admit({ env: boundaryEnv(green.sha, green.tree), git: green.git, runGate: green.runGate, fetchImpl: npm(null), policy: policy() });
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

// ---- B2: the hosted pack is bound by content, and the signed swap helper by the sign-swap job ------------------------------------
const sha256 = (data) => crypto.createHash("sha256").update(data).digest("hex");
const PKG = [{ path: "package/package.json", data: '{"name":"@rightkit/release"}' }, { path: "package/bin/cli.mjs", data: "#!/usr/bin/env node\n", mode: 0o755 }, { path: "package/lib/a.mjs", data: "export const a = 1;\n" }];
const helperEntries = (helper, receipt) => [{ path: SWAP_HELPER_ARCHIVE_BIN, data: helper, mode: 0o755 }, ...(receipt === undefined ? [] : [{ path: SWAP_HELPER_ARCHIVE_RECEIPT, data: receipt }])];
/** A qualification and manifest for one npm unit, from the archive the local gate packed (`local`); `hosted` is what the pack job sealed. */
function bindFixture({ name = "@rightkit/release", local, hosted, helper = "signed-by-ci", swap = true, over = {} }) {
  const digest = npmContentDigest(tgzOf(local), { exclude: exclusionsFor(name) });
  const q = qual({ kind: "npm", name, contentSha256: digest.contentSha256, ...(exclusionsFor(name).length ? { contentExcluded: Object.keys(digest.excluded) } : {}), artifactSha256: sha256(tgzOf(local)), ...over });
  const qualification = readQualificationInput(JSON.stringify(q), { sha: SHA, registry: "npm", units: [name] });
  const sealed = tgzOf(hosted);
  const manifest = { schema: 1, registry: "npm", commit: SHA, units: [{ name, version: "0.6.7", file: "u.tgz", sha256: sha256(sealed) }] };
  return { qualification, manifest, args: { manifest, qualification, packed: () => sealed, ...(swap ? { swapSha256: `${sha256(helper)}  rightkit-swap\n` } : {}), listing: (entry) => `listing of ${entry.file}` } };
}

test("bind: a crate is bound by its sha256; a mismatch stops with both digests and the hosted listing", () => {
  const q = readQualificationInput(JSON.stringify(qual({ kind: "cargo", name: "rightkit-qa", version: "0.9.2" })), { sha: SHA, registry: "crates", units: ["rightkit-qa"] });
  const manifest = { schema: 1, registry: "crates", commit: SHA, units: [{ name: "rightkit-qa", version: "0.9.2", file: "rightkit-qa-0.9.2.crate", sha256: "d".repeat(64) }] };
  assert.deepEqual(bindPackedToQualification({ manifest, qualification: q }), { unit: "rightkit-qa", version: "0.9.2", sha256: "d".repeat(64), file: "rightkit-qa-0.9.2.crate" });
  const other = { ...manifest, units: [{ ...manifest.units[0], sha256: "e".repeat(64) }] };
  assert.throws(() => bindPackedToQualification({ manifest: other, qualification: q, listing: (entry) => `listing of ${entry.file}` }), (e) => /hosted sha256 e{64}, qualified d{64}/.test(e.message) && /no repack/.test(e.message) && /listing of rightkit-qa-0.9.2.crate/.test(e.message));
  assert.throws(() => bindPackedToQualification({ manifest: { ...manifest, commit: OTHER }, qualification: q }), /sealed manifest is for bbbbbbbbbbbb/);
  assert.throws(() => bindPackedToQualification({ manifest: { ...manifest, units: [] }, qualification: q }), /no entry for rightkit-qa/);
  assert.throws(() => bindPackedToQualification({ manifest: { ...manifest, units: [{ ...manifest.units[0], version: "0.9.3" }] }, qualification: q }), /is 0.9.3, the qualification is 0.9.2/);
  assert.throws(() => readQualificationInput("not json", { sha: SHA, registry: "npm", units: ["@rightkit/release"] }), /not JSON/);
  assert.throws(() => readQualificationInput(JSON.stringify({ ...qual(), artifactSha256: "x" }), { sha: SHA, registry: "npm", units: ["@rightkit/release"] }), /names no artifact sha256/);
});

test("bind (B2): an npm unit is bound by content; a re-signed helper with no .sign.json passes, any other differing entry refuses, and gzip and order do not matter", () => {
  const local = [...PKG, ...helperEntries("signed-locally", '{"signed":"locally"}')];
  const hosted = [...PKG, ...helperEntries("signed-by-ci")];
  const ok = bindPackedToQualification(bindFixture({ local, hosted }).args);
  assert.equal(ok.unit, "@rightkit/release"); assert.equal(ok.swapHelperSha256, sha256("signed-by-ci")); assert.match(ok.contentSha256, /^[0-9a-f]{64}$/);
  assert.notEqual(ok.sha256, ok.qualifiedSha256, "the archives differ as bytes: only their content is equated");
  // Another gzip level and entry order is the same content.
  const sealedAgain = bindFixture({ local, hosted: [...hosted].reverse() });
  sealedAgain.args.packed = () => tgzOf([...hosted].reverse(), 1);
  assert.equal(bindPackedToQualification(sealedAgain.args).contentSha256, ok.contentSha256);
  // Any other differing entry refuses, with both content digests and the listing; nothing is repacked.
  for (const [what, entries] of [["a changed file", hosted.map((e) => (e.path.endsWith("a.mjs") ? { ...e, data: "tampered" } : e))], ["a dropped exec bit", hosted.map((e) => (e.path.endsWith("cli.mjs") ? { ...e, mode: 0o644 } : e))], ["an extra file", [...hosted, { path: "package/x.js", data: "x" }]], ["a missing file", hosted.filter((e) => !e.path.endsWith("a.mjs"))]]) {
    assert.throws(() => bindPackedToQualification(bindFixture({ local, hosted: entries }).args), (e) => /does not hold the locally qualified content/.test(e.message) && /no repack/.test(e.message) && /listing of u\.tgz/.test(e.message), what);
  }
});

test("bind (B2): the sealed helper must be the sign-swap job's, carry no .sign.json, be executable and be the one the local archive recorded; exclusions are @rightkit/release's alone", () => {
  const local = [...PKG, ...helperEntries("signed-locally", '{"signed":"locally"}')];
  const hosted = [...PKG, ...helperEntries("signed-by-ci")];
  // The sealed helper differs from what sign-swap produced, or sign-swap's digest is missing or malformed.
  const f = bindFixture({ local, hosted });
  assert.throws(() => bindPackedToQualification({ ...f.args, swapSha256: `${sha256("another-helper")}  rightkit-swap\n` }), /not the sign-swap job's/);
  assert.throws(() => bindPackedToQualification({ ...f.args, swapSha256: undefined }), /needs the sign-swap job's rightkit-swap sha256/);
  assert.throws(() => bindPackedToQualification({ ...f.args, swapSha256: "not a digest" }), /needs the sign-swap job's rightkit-swap sha256/);
  // A hosted archive that holds the local signing's receipt, or no helper, or a helper that is not executable.
  assert.throws(() => bindPackedToQualification(bindFixture({ local, hosted: [...PKG, ...helperEntries("signed-by-ci", '{"signed":"locally"}')] }).args), /holds package\/bin\/darwin\/rightkit-swap\.sign\.json, the local signing's receipt/);
  assert.throws(() => bindPackedToQualification(bindFixture({ local, hosted: PKG }).args), /holds no executable package\/bin\/darwin\/rightkit-swap/);
  assert.throws(() => bindPackedToQualification(bindFixture({ local, hosted: [...PKG, { path: SWAP_HELPER_ARCHIVE_BIN, data: "signed-by-ci", mode: 0o644 }] }).args), /holds no executable/);
  // A local archive that lacked the helper (or its receipt) never qualifies: its qualification must record both excluded files.
  const noHelper = (contentExcluded) => () => readQualificationInput(JSON.stringify(qual({ contentExcluded })), { sha: SHA, registry: "npm", units: ["@rightkit/release"] });
  assert.throws(noHelper([SWAP_HELPER_ARCHIVE_RECEIPT]), /does not record that its @rightkit\/release archive held package\/bin\/darwin\/rightkit-swap and/);
  assert.throws(noHelper([SWAP_HELPER_ARCHIVE_BIN]), /does not record/);
  assert.throws(noHelper([]), /does not record/);
  assert.throws(noHelper(undefined), /does not record/);
  assert.doesNotThrow(noHelper([SWAP_HELPER_ARCHIVE_RECEIPT, SWAP_HELPER_ARCHIVE_BIN]));
  // An exclusion claimed for another unit is refused at the qualification, and a differing helper there is any other differing entry.
  assert.throws(() => readQualificationInput(JSON.stringify(qual({ name: "@rightkit/ci", version: "0.3.2", contentExcluded: HELPER_PATHS })), { sha: SHA, registry: "npm", units: ["@rightkit/ci"] }), /claims excluded archive entries for @rightkit\/ci/);
  const other = bindFixture({ name: "@rightkit/ci", local, hosted, swap: false, over: { version: "0.3.2" } });
  other.manifest.units[0].version = "0.6.7"; other.qualification.unit.version = "0.6.7";
  assert.throws(() => bindPackedToQualification(other.args), /does not hold the locally qualified content/, "the same re-signed helper in another unit's archive is a differing entry");
  // The unit's content digest is required, and is never taken from a crate's field.
  assert.throws(() => readQualificationInput(JSON.stringify(qual({ contentSha256: undefined })), { sha: SHA, registry: "npm", units: ["@rightkit/release"] }), /names no content sha256 for its npm unit/);
  assert.throws(() => readQualificationInput(JSON.stringify(qual({ contentSha256: "xyz" })), { sha: SHA, registry: "npm", units: ["@rightkit/release"] }), /names no content sha256/);
});

test("the original denied path stays denied: rightkit-dispatch without a broker job id refuses before any gh call", async () => {
  const errors = [];
  const code = await rightkitDispatch(["--workflow", "publish", "--registry", "npm", "--unit", "@rightkit/release", "--sha", SHA, "--qualification", "/nope/qualification.json"], { env: {}, gh: () => assert.fail("no gh call without a broker job"), stderr: (l) => errors.push(l), stdout: () => {} });
  assert.equal(code, 2);
  assert.match(errors.join("\n"), /no RUSTRIGHT_JOB_ID; this runs only as the broker's rightkit-dispatch adapter/);
});

// ---- the qualification is authenticated by the broker's signed receipts, not asserted ------------------------------------------
test("authenticated qualification: receipts of another key, commit, tree, unit, stage, failed or workspace-less jobs, replays and forged booleans admit nothing", async () => {
  const admitWith = (q, { gitFn = git(), sha = SHA } = {}) => admit({ env: hostedEnv({ QUALIFICATION: JSON.stringify(q), GITHUB_SHA: sha, EXPECTED_SHA: sha }), git: gitFn, runGate: greenGate(sha), fetchImpl: npm([]), policy: policy() });
  const base = { kind: "npm", name: "@rightkit/release" };
  const ok = await admitWith(qual());
  assert.deepEqual(ok.qualification.provenance, QUAL.provenance);
  // Forged booleans: the shape the gate used to send, with no signed receipt at all.
  const bare = qual(); for (const stage of ["compile", "package"]) delete bare.stages[stage].receipts;
  await assert.rejects(admitWith(bare), /compile stage carries no signed broker receipts; an unauthenticated qualification admits nothing/);
  const noPackage = qual(); noPackage.stages.package.receipts = [];
  await assert.rejects(admitWith(noPackage), /package stage carries no signed broker receipts/);
  // A key the commit does not enrol (same host name, other key), and a host it does not enrol.
  await assert.rejects(admitWith(qual({ receiptFor: { signer: makeSigner({ host: SIGNER.pins.host, brokerDigest: SIGNER.pins.brokerDigest }) } })), /does not verify against the enrolled pins.*key: the receipt was not signed by the pinned key/);
  await assert.rejects(admitWith(qual({ receiptFor: { signer: makeSigner({ host: "another-host" }) } })), /signed for host "another-host", which ci\/rk-pipeline\.json does not enroll/);
  // Pins come from the dispatched commit, never the request: no pins committed there, or another host's pins, admit nothing.
  const pinned = (pipeline) => (...args) => (args[0] === "show" && args[1] === `${SHA}:ci/rk-pipeline.json` ? pipeline : git()(...args));
  await assert.rejects(admitWith(qual(), { gitFn: pinned("not json") }), /ci\/rk-pipeline\.json is unreadable/);
  await assert.rejects(admitWith(qual(), { gitFn: pinned("{}") }), /enrolls no attestation pins/);
  await assert.rejects(admitWith(qual(), { gitFn: pinned(JSON.stringify({ attestation: { darwin: makeSigner().pins } })) }), /does not verify against the enrolled pins.*pinned key/);
  // Replay: a receipt whose nonce binds another commit, tree, unit or stage fails naturally.
  await assert.rejects(admitWith(qual({ receiptFor: { commit: OTHER } })), /nonce: the nonce is not the one this invocation sent/);
  await assert.rejects(admitWith(qual({ receiptFor: { tree: "e".repeat(40) } })), /nonce: the nonce is not the one this invocation sent/);
  await assert.rejects(admitWith(qual({ receiptFor: { name: "@rightkit/ci" } })), /nonce: the nonce is not the one this invocation sent/);
  const crossStage = qual(); crossStage.stages.package.receipts = stageReceipts({ ...base, stage: "compile", jobs: ["stolen-compile-job"] });
  await assert.rejects(admitWith(crossStage), /package receipt 1 \(job stolen-compile-job\).*nonce/);
  // The same job twice (within or across stages) is refused before anything else.
  const twice = qual(); twice.stages.package.receipts = twice.stages.compile.receipts;
  await assert.rejects(admitWith(twice), /package receipt 1: job compile-job-1 appears more than once/);
  // The qualification's tree must equal the dispatched commit's real tree (a signed receipt for a forged tree still fails).
  await assert.rejects(admitWith(qual({ tree: "e".repeat(40) })), /tree eeeeeeeeeeee, the dispatched commit's tree is cccccccccccc/);
  // A failed job, a job with no workspace, a tampered payload.
  await assert.rejects(admitWith(qual({ receiptFor: { result: { status: "FAILED", exitCode: 101 } } })), /did not succeed \(FAILED, exit 101\)/);
  await assert.rejects(admitWith(qual({ receiptFor: { payload: { request: { kind: "cargo" } }, nonceOf: ({ stageNonce }) => stageNonce } })), /records no workspace/);
  const tampered = qual(); tampered.stages.compile.receipts[0].provenance.payload.result.exitCode = 0; tampered.stages.compile.receipts[0].provenance.payload.request.workspace = "/forged";
  await assert.rejects(admitWith(tampered), /signature: signature does not verify/);
  // A receipt that is not a signed envelope at all.
  const legacy = qual(); delete legacy.stages.compile.receipts[0].provenance;
  await assert.rejects(admitWith(legacy), /signed for host undefined/);
  // The uploader refuses an admission receipt that records no verified provenance.
  const expected = { repository: "bogusyogi/rightkit", sourceSha: SHA, runId: "77", runAttempt: "1", registry: "npm", units: [{ name: "@rightkit/release", version: "0.6.7" }], allow: { hostExclusion: [], noTest: [], ignored: [], missingAssets: [] } };
  const { provenance: _dropped, ...unattested } = ok.qualification;
  assert.throws(() => verifyAdmissionReceipt({ ...ok, qualification: unattested }, expected), /no verified broker provenance/);
  // Direct unit: any number of receipts up to the cap verify; more is refused.
  const many = (n) => { const q = qual(); q.stages.compile.receipts = stageReceipts({ ...base, stage: "compile", jobs: Array.from({ length: n }, (_, i) => `many-${i}`) }); return q; };
  await assert.rejects(admitWith(many(65)), /carries 65 receipts/);
});

test("a signed, successful job that carries the stage nonce but ran another command, in another workspace, or leaves a required command unproven admits nothing", async () => {
  const admitWith = (q) => admit({ env: hostedEnv({ QUALIFICATION: JSON.stringify(q) }), git: git(), runGate: greenGate(), fetchImpl: npm([]), policy: policy() });
  const base = { kind: "npm", name: "@rightkit/release" };
  const ws = "/work/rightkit";
  const compile = stageRows({ ...base, stage: "compile" }), pack = stageRows({ ...base, stage: "package" });
  assert.equal(compile.length, 2, "npm compile: the workspace build and the unit's test");
  assert.deepEqual(compile.map(argvOf), [["-r", "--workspace-concurrency=1", "run", "build"], ["--dir", "packages/release", "run", "test"]]);
  // @rightkit/release is the swap helper: it builds and signs a binary into its own directory of the snapshot, then packs from there (a relative --dir).
  // As a broker host receives them (carried-cargo.mjs): no --manifest-path (the call runs from the crate's directory), `--locked` on the build.
  assert.deepEqual(pack.map(argvOf), [["build", "--locked", "--release", "-p", "rightkit-fs", "--features", "fs", "--bin", "rightkit-swap"], ["metadata", "--format-version", "1", "--no-deps"], ["--dir", "packages/release", "pack", "--pack-destination", "/tmp/scratch"]]);
  assert.deepEqual(pack.map((row) => row.cwd), ["crates/rightkit-fs", "crates/rightkit-fs", undefined], "and from that directory");
  const put = (stage, receipts) => { const q = qual(); q.stages[stage].receipts = receipts; return q; };
  // Every command of the stage is proven by its own job: the full set is admitted.
  assert.deepEqual((await admitWith(qual())).qualification.provenance.compile.map((p) => p.requestedArgv), compile.map(argvOf));
  // Multi-command omission: one required job missing refuses, naming the command (either one).
  const [build, test] = stageReceipts({ ...base, stage: "compile" });
  await assert.rejects(admitWith(put("compile", [test])), /no signed receipt of the compile stage's pnpm -r --workspace-concurrency=1 run build \(1 of 2 required command\(s\) missing\)/);
  await assert.rejects(admitWith(put("compile", [build])), /no signed receipt of the compile stage's pnpm --dir packages\/release run test \(1 of 2 required command\(s\) missing\)/);
  // The same job twice, or one command run twice with another left out, is not coverage.
  const [build2] = stageReceipts({ ...base, stage: "compile", jobs: ["build-again"], rows: [compile[0]] });
  await assert.rejects(admitWith(put("compile", [build, build2])), /compile job build-again ran .* not \(or no longer\) a command/);
  // Wrong command with the right nonce, signature and success: another argv, another unit's directory, an extra flag, another adapter or kind.
  for (const [name, mutate] of [
    ["another argv", (r) => ({ ...r, requestedArgv: ["--dir", "packages/release", "run", "lint"] })],
    ["another unit's directory", (r) => ({ ...r, requestedArgv: ["--dir", "packages/ci", "run", "test"] })],
    ["an extra flag", (r) => ({ ...r, requestedArgv: [...r.requestedArgv, "--ignore-scripts"] })],
    ["another adapter", (r) => ({ ...r, adapter: "rightkit-node" })],
    ["another request kind", ({ adapter: _a, ...r }) => ({ ...r, kind: "cargo" })],
  ]) await assert.rejects(admitWith(put("compile", stageReceipts({ ...base, stage: "compile", requestFor: (row, w, i) => (i === 1 ? mutate(requestOf(row, w)) : requestOf(row, w)) }))), /is not \(or no longer\) a command of the compile stage of npm:@rightkit\/release/, name);
  const badPack = (requestedArgv) => put("package", stageReceipts({ ...base, stage: "package", requestFor: (row, w, i) => ({ ...requestOf(row, w), ...(i === 2 ? { requestedArgv } : {}) }) }));
  await assert.rejects(admitWith(badPack(["--dir", "packages/release", "pack", "--pack-destination", "relative/out"])), /not \(or no longer\) a command of the package stage/, "a pack destination must be an absolute scratch path");
  await assert.rejects(admitWith(badPack(["--dir", "/tmp/scratch", "pack", "--pack-destination", "/tmp/scratch"])), /not \(or no longer\) a command of the package stage/, "an absolute pack directory (the old scratch copy) is no longer the stage's command: the pack runs from the snapshot");
  // A compile command is not accepted as the package stage's.
  await assert.rejects(admitWith(put("package", stageReceipts({ ...base, stage: "package", requestFor: (_row, w) => requestOf(compile[1], w) }))), /not \(or no longer\) a command of the package stage/);
  // Wrong workspace: a different workspaceRoot, or two checkouts within one qualification. The checkout's own name is no evidence (B9): the
  // signed snapshot evidence is, so a checkout not named rightkit is admitted when every job shows the same one.
  await assert.rejects(admitWith(put("package", stageReceipts({ ...base, stage: "package", workspace: "/work/other-repo" }))), /another workspace than the qualification's other jobs/);
  await assert.rejects(admitWith(put("package", stageReceipts({ ...base, stage: "package", requestFor: (row, w) => ({ ...requestOf(row, w), workspaceRoot: "/work/rightkit/sub" }) }))), /not in one checkout/);
  assert.ok((await admitWith(qual({ receiptFor: { workspace: "/Volumes/D/claude/checkout-x" } }))).qualification.provenance, "a checkout not named rightkit passes");
  // The swap helper's calls are bound to the crate they build: the same argv run from another crate's directory, or from the root, is none of the stage's commands.
  const fromDir = (dir) => put("package", stageReceipts({ ...base, stage: "package", requestFor: (row, w, i) => (i === 0 ? { ...requestOf(row, w), workspace: `${w}${dir}`, workspaceRoot: `${w}${dir}` } : requestOf(row, w)) }));
  await assert.rejects(admitWith(fromDir("/crates/rightkit-other")), /not \(or no longer\) a command of the package stage/, "another crate's directory is none of the stage's commands");
  await assert.rejects(admitWith(fromDir("")), /not \(or no longer\) a command of the package stage/, "run from the repository root instead of the crate");
  const split = qual(); split.stages.package.receipts = stageReceipts({ ...base, stage: "package", workspace: "/elsewhere/rightkit" });
  await assert.rejects(admitWith(split), /another workspace than the qualification's other jobs/);
  assert.ok((await admitWith(put("package", stageReceipts({ ...base, stage: "package", workspace: "/work/rightkit/" })))).qualification.provenance, "a trailing separator is the same workspace");
  // A cargo unit owes a receipt for fmt, tests, clippy and checks alike; dropping any one refuses.
  const cargo = { kind: "cargo", name: "rightkit-qa" };
  const rows = stageRows({ ...cargo, stage: "compile" });
  assert.ok(rows.length >= 3 && argvOf(rows[0])[0] === "fmt");
  const cargoQual = (dropped) => { const q = qual({ ...cargo, version: "0.9.2" }); q.stages.compile.receipts = q.stages.compile.receipts.filter((_, i) => i !== dropped); return q; };
  const cargoEnv = (q) => ({ ...ACTIONS, ADMISSION: "broker", REQUEST_ID: "job-1", EXPECTED_SHA: SHA, REGISTRY: "crates", UNITS: "rightkit-qa", QUALIFICATION: JSON.stringify(q), ALLOW_HOST_EXCLUSION: "", ALLOW_NO_TEST: "", ALLOW_IGNORED: "", ALLOW_MISSING_ASSETS: "" });
  const crateGit = (...args) => (args[0] === "show" && args[1] === `${SHA}:crates/rightkit-qa/Cargo.toml` ? '[package]\nname = "rightkit-qa"\nversion = "0.9.2"\n' : git()(...args));
  const crate = (q) => admit({ env: cargoEnv(q), git: crateGit, runGate: greenGate(), fetchImpl: async () => ({ status: 404, ok: false }), policy: policy() });
  assert.deepEqual((await crate(cargoQual(-1))).versions, { "rightkit-qa": "0.9.2" });
  for (let dropped = 0; dropped < rows.length; dropped += 1) await assert.rejects(crate(cargoQual(dropped)), new RegExp(`no signed receipt of the compile stage's cargo ${argvOf(rows[dropped])[0]} .*1 of ${rows.length} required`), `dropped ${dropped}`);
  // The pack needs its metadata job too (either spelling).
  const packRows = stageRows({ ...cargo, stage: "package" });
  assert.equal(packRows.length, 2);
  const noMeta = qual({ ...cargo, version: "0.9.2" }); noMeta.stages.package.receipts = noMeta.stages.package.receipts.slice(0, 1);
  await assert.rejects(crate(noMeta), /no signed receipt of the package stage's cargo metadata/);
  // Artifact: the broker signs no digest of the packed bytes (payload.outputDigest is the SHA-256 of the job's stdout and stderr, and a
  // declared output's content is listed as unobserved), so artifactSha256 cannot be checked against a signed value. It is bound where
  // the hosted pack is compared with it: bytes that differ from the qualification's digest stop the run there.
  const qualification = readQualificationInput(JSON.stringify(qual()), { sha: SHA, registry: "npm", units: ["@rightkit/release"] });
  const manifest = { commit: SHA, units: [{ name: "@rightkit/release", version: "0.6.7", sha256: "e".repeat(64), file: "r.tgz" }] };
  assert.throws(() => bindPackedToQualification({ manifest, qualification, packed: () => tgzOf([{ path: "package/package.json", data: "{}" }]), swapSha256: `${"1".repeat(64)}  rightkit-swap\n` }), /does not hold the locally qualified content/);
  const signedAsArtifact = qual(); signedAsArtifact.stages.package.receipts[0].provenance.payload.outputDigest = "d".repeat(64);
  await assert.rejects(admitWith(signedAsArtifact), /signature does not verify/, "a digest claimed inside a signed payload cannot be added after signing");
  // check and qualify: `ok: true` with no evidence is accepted as the caller's word for those two stages only, and the admission says so.
  const bareCheck = qual(); bareCheck.stages.check = { ok: true }; bareCheck.stages.qualify = { ok: true };
  assert.deepEqual((await admitWith(bareCheck)).qualification.unattested, ["check", "qualify"]);
  // ...but it never substitutes for the attested stages: ok:true everywhere with no broker receipts at all admits nothing.
  const allOk = qual(); for (const stage of ["compile", "package"]) delete allOk.stages[stage].receipts;
  await assert.rejects(admitWith(allOk), /carries no signed broker receipts/);
});

// ---- B8: a carried call's nonce (a cargo call, and a pnpm adapter job, one model) is derived per call from the stage's, its directory and its argv ----
const derived = ({ row, request, stageNonce }) => (row.kind === request.kind && (row.kind === "cargo" || row.kind === "adapter") ? carriedCallNonce({ stageNonce, cwd: row.cwd ?? ".", argv: request.requestedArgv }) : stageNonce);
const RELEASE = { kind: "npm", name: "@rightkit/release" };
const admitQual = (q) => admit({ env: hostedEnv({ QUALIFICATION: JSON.stringify(q) }), git: git(), runGate: greenGate(), fetchImpl: npm([]), policy: policy() });
const withPackage = (receipts) => { const q = qual(); q.stages.package.receipts = receipts; return q; };

test("B8: a derived per-call nonce verifies; a random nonce, another directory, another stage, another unit, the stage's own nonce on a cargo call or on a pnpm job, a pnpm job's nonce derived for another directory or argv refuse", async () => {
  const pack = (nonceOf) => withPackage(stageReceipts({ ...RELEASE, stage: "package", nonceOf }));
  const [build] = stageRows({ ...RELEASE, stage: "package" });
  assert.equal(build.kind, "cargo"); assert.equal(build.cwd, "crates/rightkit-fs");
  assert.ok((await admitQual(pack(derived))).qualification.provenance, "the derived nonce, recomputed from the row's directory and argv");
  const refusedFor = (nonceOf, pattern, why) => assert.rejects(admitQual(pack((a) => (a.i === 0 ? nonceOf(a) : derived(a)))), pattern, why);
  await refusedFor(() => "a".repeat(64), /package receipt 1 .*nonce: the nonce is not the one this invocation sent/, "a random nonce");
  await refusedFor(({ request, stageNonce }) => carriedCallNonce({ stageNonce, cwd: "crates/rightkit-other", argv: request.requestedArgv }), /package receipt 1 .*nonce/, "another directory");
  await refusedFor(({ request }) => carriedCallNonce({ stageNonce: qualificationNonce({ commit: SHA, tree: TREE, unit: "npm:@rightkit/release", stage: "compile" }), cwd: "crates/rightkit-fs", argv: request.requestedArgv }), /package receipt 1 .*nonce/, "another stage's");
  await refusedFor(({ request }) => carriedCallNonce({ stageNonce: qualificationNonce({ commit: SHA, tree: TREE, unit: "npm:@rightkit/ci", stage: "package" }), cwd: "crates/rightkit-fs", argv: request.requestedArgv }), /package receipt 1 .*nonce/, "another unit's");
  await refusedFor(({ request, stageNonce }) => carriedCallNonce({ stageNonce, cwd: "crates/rightkit-fs", argv: [...request.requestedArgv, "--all-features"] }), /package receipt 1 .*nonce/, "another argv");
  // The stage nonce itself (the old regime) is not any call's nonce, a cargo call's or a pnpm job's.
  await refusedFor(({ stageNonce }) => stageNonce, /package job package-job-1 carries a nonce that is not the one its command \(cargo build .*\) owes/, "the stage's own nonce");
  await assert.rejects(admitQual(pack((a) => (a.i === 2 ? a.stageNonce : derived(a)))), /package job package-job-3 carries a nonce that is not the one its command \(pnpm .*\) owes/, "the stage's own nonce on a pnpm job");
  await assert.rejects(admitQual(pack((a) => (a.i === 2 ? carriedCallNonce({ stageNonce: a.stageNonce, cwd: "packages/release", argv: a.request.requestedArgv }) : derived(a)))), /package receipt 3 .*nonce/, "a pnpm job's nonce derived for another directory");
  await assert.rejects(admitQual(pack((a) => (a.i === 2 ? carriedCallNonce({ stageNonce: a.stageNonce, cwd: ".", argv: [...a.request.requestedArgv, "--x"] }) : derived(a)))), /package receipt 3 .*nonce/, "a pnpm job's nonce derived for another argv");
  // Two cargo calls of one directory with different argv carry different nonces: swapping them between the two jobs refuses.
  const [b1, b2] = stageReceipts({ ...RELEASE, stage: "package" }).slice(0, 2).map((r) => r.provenance.payload.nonce);
  assert.notEqual(b1, b2);
  await assert.rejects(admitQual(pack((a) => (a.i === 0 ? b2 : a.i === 1 ? b1 : derived(a)))), /package receipt 1 .*nonce/, "one call's nonce on another call");
});

// ---- B9: every receipt carries signed snapshot evidence of the dispatched commit ---------------------------------------------------------
test("B9: a receipt without snapshotEvidence, or with another commit, tree, request id, or a snapshot that moved, refuses; two snapshots or two ownership handles refuse", async () => {
  const both = (evidence) => admitQual(qual({ receiptFor: { evidence } }));
  assert.ok((await admitQual(qual())).qualification.provenance);
  await assert.rejects(both(() => undefined), /compile receipt 1 \(job compile-job-1\) carries no signed snapshotEvidence/);
  await assert.rejects(both((b) => ({ ...b, commit: { ...b.commit, sha: OTHER } })), /compile receipt 1 \(job compile-job-1\): the signed commit is not the one/);
  await assert.rejects(both((b) => ({ ...b, commit: { ...b.commit, tree: "e".repeat(40) } })), /signed commit is not the one/);
  await assert.rejects(both((b) => ({ ...b, snapshot: { ...b.snapshot, treeAtStart: "e".repeat(40) } })), /snapshot was tree e{40} at START and c{40} at exit, not c{40} throughout/);
  await assert.rejects(both((b) => ({ ...b, snapshot: { ...b.snapshot, treeAtExit: "e".repeat(40) } })), /at START and e{40} at exit/);
  await assert.rejects(both((b) => ({ ...b, requestId: "another-job" })), /signed request id is not this job/);
  await assert.rejects(both((b) => ({ ...b, snapshot: { ...b.snapshot, path: "" } })), /names no snapshot path or ownership handle/);
  await assert.rejects(both((b) => ({ ...b, ownership: {} })), /names no snapshot path or ownership handle/);
  // One snapshot and one handle for the whole qualification, within a stage and across the two.
  const pack = (evidence) => withPackage(stageReceipts({ ...RELEASE, stage: "package", evidence }));
  await assert.rejects(admitQual(pack((b, i) => (i === 1 ? { ...b, snapshot: { ...b.snapshot, path: "/snap/other" } } : b))), /package receipt 2 .*ran in the snapshot \/snap\/other, another snapshot than the qualification's other jobs \(\/snap\/rk\)/);
  await assert.rejects(admitQual(pack((b) => ({ ...b, snapshot: { ...b.snapshot, path: "/snap/other" } }))), /another snapshot than the qualification's other jobs/, "the package stage in another snapshot than the compile stage");
  await assert.rejects(admitQual(pack((b, i) => (i === 2 ? { ...b, ownership: { handle: "own-2" } } : b))), /package receipt 3 .*ownership handle own-2, another than the qualification's other jobs \(own-1\)/);
  // The commit's own name is no evidence: a checkout not named rightkit passes, and the one primary root is what the receipts agree on.
  assert.ok((await admitQual(qual({ receiptFor: { workspace: "/Volumes/D/claude/checkout-x" } }))).qualification.provenance);
  const rootOf = (workspace) => withPackage(stageReceipts({ ...RELEASE, stage: "package", requestFor: (row, w, i) => requestOf(row, i === 2 ? workspace : w) }));
  await assert.rejects(admitQual(rootOf("/work/another-root")), /package job package-job-3 ran in \/work\/another-root, another workspace than the qualification's other jobs \(\/work\/rightkit\)/);
});

// RA-21: publish-direct's sparse checkout must list the whole static import closure of its run-qualified entry, and that closure must
// load from a tree materialized exactly as the sparse list says. Both checks read the workflow's own list; nothing is mocked, and no
// workflow, credential or upload runs.
const RA21_REPO = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");
const RA21_ENTRIES = ["scripts/ci/reviewed-candidate.mjs", "scripts/ci/publish-admission.mjs", "scripts/ci/scan-receipt.mjs", "scripts/ci/upload.mjs", "scripts/ci/supplemental-proof.mjs"];
// Static relative imports only: `from "./x"`, the closing line of a multi-line import, and bare `import "./x"`. Dynamic import() edges run
// only on their own code paths and are not part of the static closure.
const RA21_STATIC_IMPORT = /(?:^|\s)(?:import|export)\b[^'"(]*?from\s*['"](\.{1,2}\/[^'"]+)['"]|^\s*\}\s*from\s*['"](\.{1,2}\/[^'"]+)['"]|^\s*import\s*['"](\.{1,2}\/[^'"]+)['"]/gm;
const ra21SparseList = () => {
  const workflow = fs.readFileSync(path.join(RA21_REPO, ".github", "workflows", "publish-direct.yml"), "utf8");
  const block = workflow.split("          sparse-checkout: |\n")[1].split("          sparse-checkout-cone-mode")[0];
  return block.split("\n").map((line) => line.trim()).filter(Boolean);
};
const ra21StaticClosure = () => {
  const seen = new Set();
  const queue = [...RA21_ENTRIES];
  while (queue.length > 0) {
    const file = queue.shift();
    if (seen.has(file)) continue;
    seen.add(file);
    const text = fs.readFileSync(path.join(RA21_REPO, file), "utf8");
    for (const match of text.matchAll(RA21_STATIC_IMPORT)) {
      const spec = match[1] ?? match[2] ?? match[3];
      // The sparse list and the entries are posix spellings; path.relative answers with backslashes on Windows, which would never match them.
      queue.push(path.relative(RA21_REPO, path.resolve(RA21_REPO, path.dirname(file), spec)).split(path.sep).join("/"));
    }
  }
  return [...seen].sort();
};

test("RA-21: publish-direct's sparse checkout lists the whole static import closure of the run-qualified entry", () => {
  const listed = new Set(ra21SparseList());
  const missing = ra21StaticClosure().filter((file) => !listed.has(file));
  assert.deepEqual(missing, [], "the sparse checkout omits modules the credentialed run loads before upload");
});

// pnpm on PATH is `pnpm.cmd` on Windows (pnpm/action-setup, as on the hosted runner), which spawn cannot start without a shell; cmd resolves it
// the way the workflow's own `run:` step does. The arguments are the fixed literals below, never input. Nothing else about the call changes:
// an absent pnpm, a wrong version or a failed install is still a failure of the test.
const ra21Pnpm = (args, options = {}) => (process.platform === "win32"
  ? spawnSync(["pnpm", ...args].join(" "), { ...options, shell: true })
  : spawnSync("pnpm", args, options));

// A sparse entry with one `*` (`packages/*/package.json`, `crates/*/Cargo.lock`) names every matching file the checkout holds.
const ra21Expand = (entry) => {
  if (!entry.includes("*")) return [entry];
  const [head, ...rest] = entry.split("*");
  const tail = rest.join("*");
  return fs.readdirSync(path.join(RA21_REPO, head), { withFileTypes: true })
    .filter((dirent) => dirent.isDirectory())
    .map((dirent) => `${head}${dirent.name}${tail}`)
    .filter((file) => fs.existsSync(path.join(RA21_REPO, file)));
};

test("RA-21: after the workflow's frozen install, the run-qualified entries load from the sparse tree", { timeout: 900_000 }, () => {
  // The scratch convention of the other script tests: RIGHTKIT_TEST_SCRATCH, else the runner's temp (on the workspace's own drive, where pnpm's store
  // lives on a hosted Windows runner), else the system temp.
  const scratch = process.env.RIGHTKIT_TEST_SCRATCH ?? (process.env.GITHUB_ACTIONS === "true" ? process.env.RUNNER_TEMP : undefined) ?? os.tmpdir();
  const dir = fs.mkdtempSync(path.join(scratch, "ra21-sparse-"));
  try {
    for (const entry of ra21SparseList()) {
      for (const file of ra21Expand(entry)) {
        fs.mkdirSync(path.dirname(path.join(dir, file)), { recursive: true });
        fs.copyFileSync(path.join(RA21_REPO, file), path.join(dir, file));
      }
    }
    // publish-direct.yml installs with the pnpm that pnpm/action-setup pins to 11.25.0 (the root packageManager); run the same install here.
    const pnpmVersion = ra21Pnpm(["--version"], { encoding: "utf8" });
    assert.equal(pnpmVersion.status, 0, "pnpm must be on PATH to install the release closure the way publish-direct.yml does");
    assert.equal(pnpmVersion.stdout.trim(), "11.25.0", "the installed pnpm must be the version publish-direct.yml pins");
    const install = ra21Pnpm(["install", "--filter", "rightkit-shared", "--filter", "@rightkit/release...", "--frozen-lockfile", "--ignore-scripts"], { cwd: dir, encoding: "utf8", maxBuffer: 64 * 1024 * 1024 });
    assert.equal(install.status, 0, `the workflow's install fails in the sparse tree:\n${install.stderr.split("\n").slice(-12).join("\n")}`);
    // Every module of the static closure is evaluated, not only the entries: a file a module reads while it loads must be in the tree.
    const modules = ra21StaticClosure().map((file) => pathToFileURL(path.join(dir, file)).href);
    // Windows needs SystemRoot (and the temp variables) to start node at all; they carry no secret and no repository state.
    const systemEnv = process.platform === "win32" ? { SystemRoot: process.env.SystemRoot, windir: process.env.windir, TEMP: process.env.TEMP, TMP: process.env.TMP } : {};
    const probe = spawnSync(process.execPath, ["--input-type=module", "-e", "const urls = JSON.parse(process.env.RA21_MODULES); for (const url of urls) { try { await import(url); } catch (error) { process.stderr.write(url + '\\n'); throw error; } }"], { cwd: dir, env: { ...systemEnv, PATH: process.env.PATH ?? "", RA21_MODULES: JSON.stringify(modules) }, encoding: "utf8" });
    assert.equal(probe.status, 0, `a closure module does not load after the install:\n${probe.stderr.split("\n").slice(0, 8).join("\n")}`);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 });
  }
});

test("RA-21: every file a closure module reads at load time is in the sparse list", () => {
  // A top-level `const X = readFileSync(new URL("./NAME", import.meta.url), ...)` runs while the module evaluates; NAME must be listed.
  const topLevelData = /^(?:export )?const \w+ = (?:JSON\.parse\()?readFileSync\(\s*new URL\(\s*["']\.\/([^"']+)["'],\s*import\.meta\.url\s*\)/gm;
  const listed = new Set(ra21SparseList());
  const missing = [];
  for (const file of ra21StaticClosure()) {
    const text = fs.readFileSync(path.join(RA21_REPO, file), "utf8");
    for (const match of text.matchAll(topLevelData)) {
      const data = path.posix.join(path.posix.dirname(file), match[1]);
      if (!listed.has(data)) missing.push(`${file} reads ${data} at load`);
    }
  }
  assert.deepEqual(missing, [], "the sparse checkout omits files a closure module reads at load time");
});
