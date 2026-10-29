// Item 7 row 7.04 (signed/item7-sage-decisions-2.md Q2): inside a door source split the identity of package, qualify and the mobile upload is
// the selected commit's immutable objects plus the gate's verification of its snapshot, never the checkout's HEAD, status or branch.
//
// Evidence labels. Every test here runs real git against a temp checkout with a hand-written live door run file (what the door writes at
// startRun); no door runs, so none of this is door-level evidence. It is unit-level evidence on every host (Mac, Dell, Linux). The door's own
// start and end snapshot verification (row 7.08) is not exercised here. Real `rightkit-qa`, signing and a real package build are not either.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { spawnSync } from "./background-process.mjs";
import { acquireCommittedSource, committedSourceDataForInternalUse, resolveSelectedCommit } from "./committed-source.mjs";
import { createCommittedOwnershipEvidenceForTest, verifyOwnershipEvidenceForTest } from "./ownership-evidence.mjs";
import { ownershipCheck, preflightFailures } from "./preflight.mjs";
import { assertPrimaryReleaseCheckout } from "./release-invocation.mjs";
import { runQa } from "./release-qa-command.mjs";
import { assertSourceQaAdmission } from "./release-qa-receipt.mjs";
import {
  NO_REPOSITORY_CODE, ReleaseSourceError, assertBindsSource, assertInsideSource, assertReleaseSource, gitRootOf, isSnapshotSource, qaAdmissionSourceOptions, resolveReleaseSource, snapshotSourceIdentity, sourceRootOf,
} from "./release-source-identity.mjs";
import { SourceSplitError } from "./source-split.mjs";
import { tempRoot } from "./test-support/temp-roots.mjs";

const git = (cwd, ...args) => {
  const r = spawnSync("git", ["-C", cwd, ...args], { encoding: "utf8" });
  assert.equal(r.status, 0, `git ${args.join(" ")}: ${r.stderr}`);
  return r.stdout.trim();
};
const NONCE = "d".repeat(32);

/**
 * A checkout with commit C (the selected one) and a HEAD one commit ahead of it, plus a snapshot directory holding exactly C's files,
 * plus a live door run file for C, the way the door writes it. The checkout is left dirty (an untracked file and a changed tracked file).
 */
function fixture({ origin = "https://github.com/bogusyogi/example-app.git" } = {}) {
  const dir = fs.realpathSync(tempRoot("rk-source-identity-"));
  const checkout = path.join(dir, "checkout"); fs.mkdirSync(checkout);
  git(checkout, "init", "-q", "-b", "main"); git(checkout, "config", "user.email", "t@example.com"); git(checkout, "config", "user.name", "t");
  git(checkout, "remote", "add", "origin", origin);
  const filesAtC = {
    ".gitignore": ".right-release/\n", "source.rs": "fn at_c() { C_ONLY_MARKER; }\n",
    "right-release.config.mjs": "export default { app: 'demo', version: '1.0.0', qa: { version: '1', config: 'rightkit-qa.toml', checks: { 'smoke.launch': ['app.launch'], journey: ['journey.*'] } } };\n",
    "rightkit-qa.toml": "app = 'demo'\n", "rightkit-qa": "#!/bin/sh\n",
  };
  for (const [rel, body] of Object.entries(filesAtC)) fs.writeFileSync(path.join(checkout, rel), body);
  git(checkout, "add", "-A"); git(checkout, "commit", "-q", "-m", "C");
  const commit = git(checkout, "rev-parse", "HEAD");
  const tree = git(checkout, "rev-parse", "HEAD^{tree}");
  // HEAD moves one commit ahead of C: the checkout's HEAD is not the selected commit.
  fs.writeFileSync(path.join(checkout, "source.rs"), "fn at_head() {}\n");
  fs.writeFileSync(path.join(checkout, "right-release.config.mjs"), filesAtC["right-release.config.mjs"].replace("1.0.0", "2.0.0"));
  git(checkout, "add", "-A"); git(checkout, "commit", "-q", "-m", "HEAD ahead");
  const head = git(checkout, "rev-parse", "HEAD");
  // ... and the checkout is dirty on top of that.
  fs.writeFileSync(path.join(checkout, "source.rs"), "fn dirty() {}\n");
  fs.writeFileSync(path.join(checkout, "untracked.txt"), "x\n");
  const snapshot = path.join(dir, "snapshot"); fs.mkdirSync(snapshot);
  for (const [rel, body] of Object.entries(filesAtC)) fs.writeFileSync(path.join(snapshot, rel), body);
  const commonDir = path.join(checkout, ".git");
  const runsDir = path.join(commonDir, "rightkit-gate", "runs");
  const writeRun = (patch = {}, sourcePatch = {}) => {
    fs.mkdirSync(runsDir, { recursive: true });
    const run = { pid: process.pid, commit, tree, stage: "package", source: { checkout, snapshot, commit, tree, appDir: ".", ...sourcePatch }, ...patch };
    fs.writeFileSync(path.join(runsDir, `${NONCE}.json`), `${JSON.stringify(run)}\n`);
  };
  writeRun();
  const env = { RK_GATE_RUN: NONCE, RK_GATE_CHECKOUT: checkout };
  return { dir, checkout, snapshot, commit, tree, head, env, writeRun, runFile: path.join(runsDir, `${NONCE}.json`), filesAtC };
}

/** A spawnSync-shaped runner that records every git argv and then runs it. */
function recorder() {
  const calls = [];
  const run = (command, args, options) => { calls.push([command, ...args]); return spawnSync(command, args, options); };
  const offending = () => calls.filter((c) => c.includes("status") || c.includes("symbolic-ref") || c.some((a) => /^HEAD(\^|$|~|@)/.test(a)));
  return { calls, run, offending };
}

// ---- resolve: the two shapes, and the refusals --------------------------------------------------------------------------------------

test("[7.04] in a split the source is the selected commit and its tree from the object database: a dirty checkout with HEAD ahead changes nothing, and no status, HEAD or branch is read", () => {
  const fx = fixture();
  assert.notEqual(fx.head, fx.commit, "the fixture's HEAD is not the selected commit");
  const rec = recorder();
  const source = resolveReleaseSource({ cwd: fx.snapshot, env: fx.env, git: rec.run });
  assert.deepEqual(Object.keys(source).sort(), ["appRoot", "checkout", "checkoutAppRoot", "commit", "commonDir", "mode", "snapshot", "tree"], "the frozen record's fields");
  assert.equal(source.mode, "snapshot"); assert.equal(source.commit, fx.commit); assert.equal(source.tree, fx.tree);
  assert.equal(source.checkout, fx.checkout); assert.equal(source.snapshot, fx.snapshot); assert.equal(source.appRoot, fx.snapshot); assert.equal(source.checkoutAppRoot, fx.checkout);
  assert.equal(source.commonDir, path.join(fx.checkout, ".git"));
  assert.equal(Object.isFrozen(source), true);
  assert.equal(isSnapshotSource(source), true); assert.equal(gitRootOf(source), fx.checkout); assert.equal(sourceRootOf(source), fx.snapshot);
  // The checkout is detached AND dirty AND ahead; none of it matters to a split.
  git(fx.checkout, "checkout", "-q", "--detach");
  assert.equal(assertReleaseSource(source, { commandId: "right-release build", git: rec.run }), source);
  assert.deepEqual(rec.offending(), [], `no status, symbolic-ref or HEAD revision was run: ${JSON.stringify(rec.offending())}`);
  assert.ok(rec.calls.some((c) => c.includes(`${fx.commit}^{tree}`)), "the tree was re-derived from the object database");
});

test("[7.04] R1: with no split the checkout is the source, byte for byte as before: a dirty checkout, a detached HEAD and a non-primary worktree are refused", () => {
  const fx = fixture();
  const rec = recorder();
  const source = resolveReleaseSource({ cwd: fx.checkout, env: {}, git: rec.run });
  assert.equal(source.mode, "checkout"); assert.equal(source.commit, fx.head); assert.equal(source.repoRoot, fx.checkout); assert.equal(source.appRoot, fx.checkout);
  assert.deepEqual(Object.keys(source).sort(), ["appRoot", "commit", "mode", "repoRoot", "tree"]);
  assert.equal(isSnapshotSource(source), false); assert.equal(gitRootOf(source), fx.checkout); assert.equal(sourceRootOf(source), fx.checkout);
  assert.throws(() => assertReleaseSource(source, { commandId: "right-release qa" }), /right-release qa requires a clean working tree.*untracked\.txt/s, "R1: a dirty checkout is still refused");
  assert.doesNotThrow(() => assertReleaseSource(source, { commandId: "right-release qa", skipCleanCheck: true }));
  // Clean it: the clean checkout passes; detached HEAD is the next refusal.
  git(fx.checkout, "checkout", "--", "."); fs.rmSync(path.join(fx.checkout, "untracked.txt"));
  assert.doesNotThrow(() => assertReleaseSource(source, { commandId: "right-release qa" }));
  git(fx.checkout, "checkout", "-q", "--detach");
  assert.throws(() => assertReleaseSource(source, { commandId: "right-release qa" }), /branch-attached primary Git checkout; detached HEAD is forbidden/);
  assert.doesNotThrow(() => assertReleaseSource(source, { commandId: "right-release qa", skipPrimaryCheckout: true }), "the existing test seam still skips the primary check only");
  // A directory outside any repository is no source (the callers keep their own message).
  assert.throws(() => resolveReleaseSource({ cwd: os.tmpdir(), env: {} }), (e) => e instanceof ReleaseSourceError && e.code === NO_REPOSITORY_CODE);
});

test("[7.04] R2: a split that cannot be proven is refused with RK_SOURCE_SPLIT and never falls back to the checkout checks (no status is run)", () => {
  const fx = fixture();
  const cases = [
    ["no door run named", { RK_GATE_CHECKOUT: fx.checkout }, fx.snapshot, () => {}, /no live door run/],
    ["a run that has ended", fx.env, fx.snapshot, () => fx.writeRun({ pid: 2 ** 22 + 12345 }), /no live door run \(it has ended\)/],
    ["a run made in another checkout", fx.env, fx.snapshot, () => fx.writeRun({}, { checkout: os.tmpdir() }), /the door run was made in/],
    ["a process outside the snapshot", fx.env, fx.checkout, () => fx.writeRun(), /outside the snapshot/],
    ["a relative checkout name", { ...fx.env, RK_GATE_CHECKOUT: "relative" }, fx.snapshot, () => fx.writeRun(), /must be an absolute path/],
    ["a run with no source", fx.env, fx.snapshot, () => { const run = JSON.parse(fs.readFileSync(fx.runFile, "utf8")); delete run.source; fs.writeFileSync(fx.runFile, JSON.stringify(run)); }, /carries no source split/],
  ];
  for (const [label, env, cwd, arrange, pattern] of cases) {
    fx.writeRun(); arrange();
    const rec = recorder();
    assert.throws(() => resolveReleaseSource({ cwd, env, git: rec.run }), (e) => e instanceof SourceSplitError && e.code === "RK_SOURCE_SPLIT" && pattern.test(e.message), label);
    assert.deepEqual(rec.offending(), [], `${label}: nothing fell back to a status, HEAD or branch read`);
  }
});

test("[7.04] R3: the tree is re-derived from the object database; a forged tree or a commit the checkout does not hold is refused", () => {
  const fx = fixture();
  fx.writeRun({ tree: fx.tree }, { tree: git(fx.checkout, "rev-parse", `${fx.head}^{tree}`) });
  assert.throws(() => resolveReleaseSource({ cwd: fx.snapshot, env: fx.env }), (e) => e instanceof SourceSplitError && /object database says/.test(e.message) && /\(R3\)/.test(e.message), "another commit's tree");
  fx.writeRun({}, { tree: "e".repeat(40) });
  assert.throws(() => resolveReleaseSource({ cwd: fx.snapshot, env: fx.env }), /object database says/, "a tree that is no tree of that commit");
  fx.writeRun({ commit: "a".repeat(40) }, { commit: "a".repeat(40) });
  assert.throws(() => resolveReleaseSource({ cwd: fx.snapshot, env: fx.env }), (e) => e instanceof SourceSplitError && /cannot be read from the object database/.test(e.message), "a commit the object database does not hold");
  fx.writeRun();
  assert.equal(resolveReleaseSource({ cwd: fx.snapshot, env: fx.env }).tree, fx.tree, "the honest run is accepted");
});

test("[7.04] a hand-built source carries no identity; a source whose run ended is refused when it is asserted again", () => {
  const fx = fixture();
  const forged = Object.freeze({ mode: "snapshot", commit: fx.commit, tree: fx.tree, checkout: fx.checkout, checkoutAppRoot: fx.checkout, snapshot: fx.snapshot, appRoot: fx.snapshot, commonDir: path.join(fx.checkout, ".git") });
  assert.throws(() => assertReleaseSource(forged, { commandId: "x" }), /not resolved by resolveReleaseSource/);
  assert.throws(() => isSnapshotSource(forged), SourceSplitError);
  assert.throws(() => assertInsideSource(forged, fx.checkout), SourceSplitError);
  const source = resolveReleaseSource({ cwd: fx.snapshot, env: fx.env });
  assert.doesNotThrow(() => assertReleaseSource(source, { commandId: "right-release build" }));
  fs.rmSync(fx.runFile);
  assert.throws(() => assertReleaseSource(source, { commandId: "right-release build" }), (e) => e instanceof SourceSplitError && /no live door run/.test(e.message), "the run is gone: nothing continues from a snapshot that is no longer proven");
  // A run for another commit under the same nonce.
  fx.writeRun({ commit: fx.head, tree: git(fx.checkout, "rev-parse", `${fx.head}^{tree}`) }, { commit: fx.head, tree: git(fx.checkout, "rev-parse", `${fx.head}^{tree}`) });
  assert.throws(() => assertReleaseSource(source, { commandId: "right-release build" }), /source\.commit changed while right-release build ran/, "a run for another commit is not this source");
});

test("[7.04] R1/R2: the primary-worktree identity stays in a split, the branch-attached refusal does not", () => {
  const fx = fixture();
  git(fx.checkout, "checkout", "-q", "--detach");
  assert.throws(() => assertPrimaryReleaseCheckout(fx.checkout, {}), /detached HEAD is forbidden/, "unchanged without the split flag");
  assert.doesNotThrow(() => assertPrimaryReleaseCheckout(fx.checkout, {}, { split: true }));
  // A linked worktree of the checkout is not the primary worktree, split or not.
  const linked = path.join(fx.dir, "linked");
  git(fx.checkout, "worktree", "add", "-q", "--detach", linked, fx.commit);
  assert.throws(() => assertPrimaryReleaseCheckout(linked, {}, { split: true }), /must be invoked from the primary Git worktree/);
});

// ---- R4, R5 ---------------------------------------------------------------------------------------------------------------------

test("[7.04] R4: a record for another commit or tree than the selected one is refused, a record for it is accepted", () => {
  const fx = fixture();
  const source = resolveReleaseSource({ cwd: fx.snapshot, env: fx.env });
  assert.doesNotThrow(() => assertBindsSource(source, { commit: fx.commit }, "the sealed release"));
  assert.doesNotThrow(() => assertBindsSource(source, { commit: fx.commit, tree: fx.tree }, "the sealed release"));
  assert.throws(() => assertBindsSource(source, { commit: fx.head }, "the sealed release"), (e) => e instanceof ReleaseSourceError && e.code === "RK_SOURCE_IDENTITY" && /not the selected commit/.test(e.message), "HEAD is not the selected commit");
  assert.throws(() => assertBindsSource(source, { commit: fx.commit, tree: git(fx.checkout, "rev-parse", `${fx.head}^{tree}`) }, "ownership evidence"), /not the selected commit's tree/);
  assert.throws(() => assertBindsSource(source, {}, "a resumed build"), /is for commit \(none\)/);
  assert.throws(() => assertBindsSource(source, null, "a resumed build"), /is missing/);
});

test("[7.04] R5: a config, input or QA file outside the snapshot is refused, symlinks included; the checkout route proves no containment", () => {
  const fx = fixture();
  const source = resolveReleaseSource({ cwd: fx.snapshot, env: fx.env });
  assert.doesNotThrow(() => assertInsideSource(source, path.join(fx.snapshot, "right-release.config.mjs"), "the release config"));
  assert.doesNotThrow(() => assertInsideSource(source, fx.snapshot, "the snapshot itself"));
  for (const outside of [path.join(fx.checkout, "right-release.config.mjs"), fx.checkout, path.join(fx.snapshot, "..", "checkout", "source.rs")]) {
    assert.throws(() => assertInsideSource(source, outside, "the release config"), (e) => e instanceof ReleaseSourceError && /outside the snapshot/.test(e.message) && /\(R5\)/.test(e.message), outside);
  }
  assert.throws(() => assertInsideSource(source, "", "qa.config"), /has no path/);
  try {
    fs.symlinkSync(path.join(fx.checkout, "source.rs"), path.join(fx.snapshot, "linked.rs"));
    assert.throws(() => assertInsideSource(source, path.join(fx.snapshot, "linked.rs"), "qa.config"), /outside the snapshot/, "a link that leads into the checkout");
  } catch (error) { if (!["EPERM", "EACCES", "UNKNOWN"].includes(error.code)) throw error; /* symlinks need a privilege on this Windows host */ }
  const checkoutSource = resolveReleaseSource({ cwd: fx.checkout, env: {} });
  assert.doesNotThrow(() => assertInsideSource(checkoutSource, path.join(fx.checkout, "anything"), "x"), "the checkout route is untouched");
});

test("[7.04] the QA identity of a split is the selected commit, its tree and the origin's repository identity, read without HEAD", () => {
  const fx = fixture();
  const rec = recorder();
  const source = resolveReleaseSource({ cwd: fx.snapshot, env: fx.env });
  const identity = snapshotSourceIdentity(source, rec.run);
  assert.deepEqual(identity, { commit: fx.commit, tree: fx.tree, repositoryIdentity: "https://github.com/bogusyogi/example-app" });
  assert.deepEqual(rec.offending(), []);
  assert.throws(() => snapshotSourceIdentity(resolveReleaseSource({ cwd: fx.checkout, env: {} })), ReleaseSourceError);
});

// ---- committed-source: the selected commit, never HEAD -------------------------------------------------------------------------

test("[7.04] acquireCommittedSource reads the selected commit's blobs when asked and HEAD otherwise; only a full object id of a commit is accepted", () => {
  const fx = fixture();
  const selected = acquireCommittedSource(fx.checkout, { commit: fx.commit });
  assert.equal(selected.commit, fx.commit); assert.equal(selected.tree, fx.tree);
  const at = (handle, rel) => committedSourceDataForInternalUse(handle).entries.find((e) => e.rel === rel)?.bytes?.toString("utf8");
  assert.equal(at(selected, "source.rs"), "fn at_c() { C_ONLY_MARKER; }\n", "the selected commit's blob, not HEAD's and not the dirty checkout's");
  const headHandle = acquireCommittedSource(fx.checkout);
  assert.equal(headHandle.commit, fx.head, "without `commit` the source is HEAD, as before");
  assert.equal(at(headHandle, "source.rs"), "fn at_head() {}\n");
  assert.equal(acquireCommittedSource(fx.checkout, { commit: fx.commit, expectedTree: fx.tree }).tree, fx.tree);
  for (const bad of ["HEAD", "main", fx.commit.slice(0, 12), "--help", `${fx.commit}^`, `${fx.commit}^{tree}`, fx.tree, "a".repeat(40), ""]) {
    assert.throws(() => acquireCommittedSource(fx.checkout, { commit: bad }), (e) => e.code === "COMMITTED_SOURCE_INCOMPLETE", `refused: ${JSON.stringify(bad)}`);
  }
  assert.throws(() => acquireCommittedSource(fx.checkout, { commit: fx.commit, expectedCommit: fx.head }), /expected commit is not the selected commit/);
  assert.throws(() => acquireCommittedSource(fx.checkout, { commit: fx.commit, expectedTree: git(fx.checkout, "rev-parse", `${fx.head}^{tree}`) }), /expected tree does not match commit/);
  assert.deepEqual(resolveSelectedCommit(fx.checkout, fx.commit), { commit: fx.commit, tree: fx.tree, objectFormat: "sha1" });
  assert.throws(() => resolveSelectedCommit(fx.checkout, "HEAD"), /not a full object id/);
});

// ---- ownership evidence ----------------------------------------------------------------------------------------------------------

const trusted = () => () => ({
  enrolled: true, pending: false, protection: { ok: true },
  trust: { schema: 1, revokedKeyIds: [], hosts: {}, verifiedTrackingEvidence: [], minimumScannerVersion: "0.0.0", minimumCatalogVersion: "0.0.0", provenance: [], repositories: [{ repositoryIdentity: "https://github.com/bogusyogi/example-app", roots: ["."], visibility: "private" }] },
});
const catalog = (fx) => {
  const file = path.join(fx.dir, "catalog.json");
  fs.writeFileSync(file, JSON.stringify({ schema: 1, version: "1", capabilities: [{ package: "sample", capability: "sample", tier: "block", fingerprints: [{ type: "source", id: "marker", pattern: "C_ONLY_MARKER" }] }] }));
  return file;
};

test("[7.04] ownership evidence in a split is the selected commit's committed source: a dirty checkout and a HEAD ahead of it are irrelevant, and its binding is verified against the same commit", () => {
  const fx = fixture();
  const source = resolveReleaseSource({ cwd: fx.snapshot, env: fx.env });
  const options = { scanRoot: fx.snapshot, manifestPath: catalog(fx), trustLoader: trusted(), requireCleanCheckout: true };
  // Without the split the same call demands the clean checkout (R1).
  assert.throws(() => createCommittedOwnershipEvidenceForTest(fx.checkout, { ...options, scanRoot: fx.checkout }), /protected release checkout has tracked or untracked changes/);
  const built = createCommittedOwnershipEvidenceForTest(fx.checkout, { ...options, releaseSource: source });
  assert.equal(built.evidence.source, "committed-git-tree");
  assert.equal(built.evidence.commit, fx.commit, "the selected commit, not HEAD"); assert.equal(built.evidence.tree, fx.tree);
  assert.deepEqual(built.result.findings.map((f) => f.file), ["source.rs"], "the selected commit's bytes were scanned (the marker exists only there)");
  assert.equal(built.result.metadata.commit, fx.commit);
  const verified = verifyOwnershipEvidenceForTest(built.evidence, fx.checkout, { ...options, releaseSource: source });
  assert.ok(verified.ok || verified.reason === "current ownership scan fails or has incomplete coverage", `the re-scan binds the same commit, tree and content (a marker finding is a scan result, not an identity change): ${verified.reason}`);
  // Identity: evidence of HEAD's commit does not verify against the selected one, and the reverse.
  const headEvidence = createCommittedOwnershipEvidenceForTest(fx.checkout, { ...options, requireCleanCheckout: false, scanRoot: fx.checkout });
  assert.equal(headEvidence.evidence.commit, fx.head);
  assert.match(verifyOwnershipEvidenceForTest(headEvidence.evidence, fx.checkout, { ...options, releaseSource: source }).reason, /ownership evidence (commit|tree|contentDigest) changed after build/, "R4: the evidence commit differs from the split commit");
});

test("[7.04] ownership evidence refuses a hand-built source, another git root, and a host that can only give advisory evidence", () => {
  const fx = fixture();
  const options = { scanRoot: fx.snapshot, manifestPath: catalog(fx), trustLoader: trusted() };
  const source = resolveReleaseSource({ cwd: fx.snapshot, env: fx.env });
  assert.throws(() => createCommittedOwnershipEvidenceForTest(fx.checkout, { ...options, releaseSource: { ...source } }), /not resolved by resolveReleaseSource/);
  assert.throws(() => createCommittedOwnershipEvidenceForTest(fx.snapshot, { ...options, releaseSource: source }), /the git root is the run's checkout/);
  const pending = { ...options, trustLoader: () => ({ enrolled: false, pending: true, protection: { ok: false, reason: "external trust file is absent" }, trust: null }) };
  assert.throws(() => createCommittedOwnershipEvidenceForTest(fx.checkout, { ...pending, releaseSource: source }), /needs the committed-source scan/, "fail closed: no advisory scan of a mutable tree in a split");
});

test("[7.04] preflight ownership: the factory receives the source, and evidence for another commit than the selected one is refused whatever produced it", () => {
  const fx = fixture();
  const source = resolveReleaseSource({ cwd: fx.snapshot, env: fx.env });
  const evidence = (commit, tree) => ({ ok: true, evidence: { commit, tree }, result: { covered: [] }, floor: { ok: true } });
  let received = null;
  const good = ownershipCheck(fx.checkout, { scanRoot: fx.snapshot, releaseSource: source, evidenceFactory: (...args) => { received = args; return evidence(fx.commit, fx.tree); } });
  assert.equal(good.status, "ok", good.detail); assert.equal(good.evidence.commit, fx.commit);
  assert.equal(received.length, 4); assert.equal(received[0], fx.checkout); assert.equal(received[3], source);
  for (const [label, factory] of [["HEAD's commit", () => evidence(fx.head, fx.tree)], ["another tree", () => evidence(fx.commit, "f".repeat(40))], ["no commit", () => evidence(undefined, undefined)]]) {
    const check = ownershipCheck(fx.checkout, { scanRoot: fx.snapshot, releaseSource: source, evidenceFactory: factory });
    assert.equal(preflightFailures([check]).length, 1, label); assert.match(check.detail, /not the selected commit/, label);
  }
  // No source: the factory is called with exactly its three arguments, as always.
  ownershipCheck(fx.checkout, { evidenceFactory: (...args) => { received = args; return evidence(fx.head, fx.tree); } });
  assert.equal(received.length, 3);
});

// ---- right-release qa (qualify) --------------------------------------------------------------------------------------------------

const store = { read: () => "unit-test-key" };
const passingEvidence = () => ({ schemaVersion: 4, runId: "r1", status: "passed", finishedAt: "2026-10-09T00:00:00Z", checks: [
  { name: "app.launch", status: "passed" }, { name: "journey.open-doc", status: "passed" }, { name: "journey.save", status: "passed" },
] });

test("[7.04] qa --stage source from a split seals the selected commit's source: config and QA plan come from the snapshot, HEAD ahead and a dirty checkout change nothing, and no status or HEAD is read", async () => {
  const fx = fixture();
  const evidenceFile = path.join(fx.dir, "evidence.json"); fs.writeFileSync(evidenceFile, JSON.stringify(passingEvidence()));
  const rec = recorder();
  const seen = [];
  const deps = (extra = {}) => ({
    env: { ...process.env, ...fx.env }, gitRunner: rec.run, quiet: true, credentialStore: store, policyBundleDigest: "b".repeat(64), runnerFile: path.join(fx.snapshot, "rightkit-qa"),
    spawnRunner: (_command, args, options) => { seen.push({ args, cwd: options.cwd, commit: options.env.RIGHT_RELEASE_QA_COMMIT }); return { status: 0, stdout: `[qa] 3 passed, 0 failed, 0 skipped; evidence ${evidenceFile}\n`, stderr: "" }; },
    ...extra,
  });
  const { receipt, file } = await runQa({ stage: "source", platform: "mac", configPath: path.join(fx.snapshot, "right-release.config.mjs"), deps: deps() });
  assert.equal(receipt.status, "passed");
  assert.equal(receipt.commit, fx.commit, "the receipt names the selected commit, not HEAD"); assert.equal(receipt.tree, fx.tree);
  assert.equal(receipt.repositoryIdentity, "https://github.com/bogusyogi/example-app");
  assert.equal(receipt.releaseId, `demo-1.0.0-${fx.commit.slice(0, 8)}`, "the snapshot's config (version 1.0.0), not the checkout's HEAD config (2.0.0)");
  assert.ok(file.startsWith(fx.checkout), "the receipt lives in the checkout's .right-release");
  assert.equal(seen.length, 1); assert.equal(seen[0].cwd, fx.snapshot, "the runner runs in the snapshot's app root"); assert.equal(seen[0].commit, fx.commit);
  assert.ok(seen[0].args.includes(path.join(fx.snapshot, "rightkit-qa.toml")), "the QA plan is the snapshot's");
  assert.deepEqual(rec.offending(), [], `no status, symbolic-ref or HEAD revision from right-release qa: ${JSON.stringify(rec.offending())}`);
  // The build's source-QA admission verifies this receipt. Against HEAD (the checkout route's identity) it is for another commit; with the
  // split's identity options it is the selected commit's receipt and admits.
  const admission = { repoRoot: fx.checkout, releaseId: receipt.releaseId, platform: "mac", configName: "right-release.config.mjs", credentialStore: store, policyBundleDigest: "b".repeat(64),
    releaseConfigSha256: createHash("sha256").update(fs.readFileSync(path.join(fx.snapshot, "right-release.config.mjs"))).digest("hex") };
  assert.throws(() => assertSourceQaAdmission(admission), /receipt is for commit .*, current commit is/, "the checkout's HEAD is not the selected commit");
  const options = qaAdmissionSourceOptions(resolveReleaseSource({ cwd: fx.snapshot, env: fx.env }), rec.run);
  assert.equal(options.source.commit, fx.commit); assert.equal(options.source.tree, fx.tree); assert.ok(options.closure.digest);
  assert.equal(assertSourceQaAdmission({ ...admission, ...options }).digest, receipt.digest);
  assert.deepEqual(qaAdmissionSourceOptions(resolveReleaseSource({ cwd: fx.checkout, env: {} })), {}, "the checkout route passes nothing and reads HEAD as before");
});

test("[7.04] qa under a split refuses a config outside the snapshot (R5), a split it cannot prove (R2), and a QA plan or runner that leaves the snapshot, starting no runner; R1: a dirty checkout without a split is still refused", async () => {
  const fx = fixture();
  const noRunner = () => { assert.fail("no runner may start"); };
  const base = { env: { ...process.env, ...fx.env }, quiet: true, credentialStore: store, policyBundleDigest: "b".repeat(64), spawnRunner: noRunner };
  await assert.rejects(runQa({ stage: "source", platform: "mac", configPath: path.join(fx.checkout, "right-release.config.mjs"), deps: { ...base, runnerFile: path.join(fx.snapshot, "rightkit-qa") } }),
    (e) => e.code === "RK_SOURCE_SPLIT" && /outside the snapshot/.test(e.message), "R5: the config sits in the checkout");
  fs.rmSync(fx.runFile);
  await assert.rejects(runQa({ stage: "source", platform: "mac", configPath: path.join(fx.snapshot, "right-release.config.mjs"), deps: { ...base, runnerFile: path.join(fx.snapshot, "rightkit-qa") } }),
    (e) => e.code === "RK_SOURCE_SPLIT" && /no live door run/.test(e.message), "R2: the run is gone, and nothing falls back to the checkout");
  fx.writeRun();
  // A QA plan that points out of the snapshot.
  fs.writeFileSync(path.join(fx.snapshot, "right-release.config.mjs"), fx.filesAtC["right-release.config.mjs"].replace("config: 'rightkit-qa.toml'", `config: '../checkout/rightkit-qa.toml'`));
  await assert.rejects(runQa({ stage: "source", platform: "mac", configPath: path.join(fx.snapshot, "right-release.config.mjs"), deps: { ...base, runnerFile: path.join(fx.snapshot, "rightkit-qa") } }),
    (e) => /qa\.config .* is outside the snapshot/.test(e.message), "R5: the QA plan in the checkout");
  // R1: no split, dirty checkout.
  await assert.rejects(runQa({ stage: "source", platform: "mac", configPath: path.join(fx.checkout, "right-release.config.mjs"), deps: { ...base, env: { ...process.env, RK_GATE_RUN: "", RK_GATE_CHECKOUT: "" }, skipPrimaryCheckout: true, runnerFile: path.join(fx.checkout, "rightkit-qa") } }),
    /right-release qa requires a clean working tree/);
});

// ---- the mobile upload (ios, android): authorize ----------------------------------------------------------------------------------

const IOS_DISTRIBUTION = { provider: "app-store-connect", appId: "6806854037", bundleId: "app.heardright.ios.native", groups: "all" };

test("[7.04] the mobile upload's authorize takes split identity: with HEAD ahead and the checkout dirty it passes the identity gate and goes on to the sealed release; an unprovable split and a config outside the snapshot are refused first", async () => {
  const { authorizeIosUpload } = await import("./ios-upload.mjs");
  const { authorizePlayUpload } = await import("./android-play-upload.mjs");
  const fx = fixture();
  const env = { ...process.env, ...fx.env };
  fs.writeFileSync(path.join(fx.snapshot, "ios.config.mjs"), `export default ${JSON.stringify({ app: "demo", version: "1.0.0", targets: { ios: { distribution: IOS_DISTRIBUTION } } })};\n`);
  fs.writeFileSync(path.join(fx.snapshot, "android.config.mjs"), `export default ${JSON.stringify({ app: "demo", version: "1.0.0" })};\n`);
  // Identity gate passed (no clean-tree or HEAD refusal although the checkout is dirty and ahead): the next refusals are the seal's and the config's.
  await assert.rejects(authorizeIosUpload({ releaseId: "demo-1.0.0-abcd1234", cwd: fx.snapshot, configName: "ios.config.mjs", env }), (e) => /^PUBLICATION_/.test(e.code), "ios: the sealed release is looked up in the checkout's .right-release and is absent");
  await assert.rejects(authorizePlayUpload({ releaseId: "demo-1.0.0-abcd1234", cwd: fx.snapshot, configName: "android.config.mjs", env }), (e) => e.code === "PLAY_CONFIG_INVALID", "android: the identity gate passed, the config has no distribution");
  for (const [label, authorize, configName] of [["ios", authorizeIosUpload, "ios.config.mjs"], ["android", authorizePlayUpload, "android.config.mjs"]]) {
    // R2: the process is not inside the snapshot of the run it names.
    await assert.rejects(authorize({ releaseId: "demo-1.0.0-abcd1234", cwd: fx.checkout, configName: path.join(fx.snapshot, configName), env }), (e) => e.code === "RK_SOURCE_SPLIT" && /outside the snapshot/.test(e.message), `${label} R2`);
    // R5: the config is the checkout's.
    await assert.rejects(authorize({ releaseId: "demo-1.0.0-abcd1234", cwd: fx.snapshot, configName: path.join(fx.checkout, "right-release.config.mjs"), env }), (e) => e.code === "RK_SOURCE_IDENTITY" && /outside the snapshot/.test(e.message), `${label} R5`);
    // R1: no split, a dirty checkout: the clean-tree refusal, as always.
    await assert.rejects(authorize({ releaseId: "demo-1.0.0-abcd1234", cwd: fx.checkout, configName: "right-release.config.mjs", env: { ...process.env, RK_GATE_RUN: "", RK_GATE_CHECKOUT: "" } }), /requires a clean working tree/, `${label} R1`);
  }
});
