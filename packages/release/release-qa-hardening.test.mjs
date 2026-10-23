// ADR-0001 item 10 (W4-A): release admission hardening and the per-app warn -> refuse switch.
import assert from "node:assert/strict";
import { cpSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { admitInstalledQa, admitSourceQa, appOfReleaseId } from "./release-qa-audit.mjs";
import { runQa } from "./release-qa-command.mjs";
import { criticalJourneyIds, hardenQaReceipt, validateJourneysReceipt } from "./release-qa-hardening.mjs";
import {
  checkPolicyTransition, loadAdmissionPolicy, QA_ADMISSION_POLICY_FILE, QaPolicyError, setAdmissionMode, validateAdmissionPolicy,
} from "./release-qa-policy.mjs";
import { loadQaFloor, QaAdmissionError, sourceIdentity, validateAppQaConfig, validateQaFloor } from "./release-qa-receipt.mjs";
import {
  accepting, gateOpts, gitFixture, here, journeysReceiptJson, MANIFEST_TOML, OTHER_RUNNER_SHA, policyWith, refusing, releaseIdOf, RUNNER_SHA, SHA, writeHardenedReceipt,
} from "./release-qa-fixtures.mjs";

const REFUSE = { demo: { mode: "refuse", since: "2026-10-24", set_by: "adrian" } };
const noWarn = () => assert.fail("must not warn");

function withRepo(body) {
  const root = gitFixture();
  try { return body(root, releaseIdOf(root)); } finally { rmSync(root, { recursive: true, force: true }); }
}

// Run one source admission in warn mode (expects a recorded refusal) and in refuse mode (expects a throw).
function refusedBoth(root, releaseId, pattern, platform = "mac") {
  const lines = [];
  const warned = admitSourceQa(gateOpts(root, releaseId, platform), { warn: (line) => lines.push(line), policy: policyWith() });
  assert.equal(warned.status, "rejected");
  assert.match(warned.reason, pattern);
  assert.equal(lines.length, 1);
  assert.throws(() => admitSourceQa(gateOpts(root, releaseId, platform), { warn: noWarn, policy: policyWith({ apps: REFUSE }) }),
    (error) => error instanceof QaAdmissionError && pattern.test(error.message) && /policy refuse since 2026-10-24 by adrian/.test(error.message));
}

test("a hardened receipt for this platform and release passes in warn and in refuse mode", () => withRepo((root, releaseId) => {
  const { receipt } = writeHardenedReceipt(root, { releaseId });
  for (const apps of [{}, REFUSE]) {
    const sealed = admitSourceQa(gateOpts(root, releaseId), { warn: noWarn, policy: policyWith({ apps }) });
    assert.equal(sealed.status, "passed");
    assert.equal(sealed.sourceReceiptDigest, receipt.digest);
  }
}));

test("wrong platform is refused (a windows receipt cannot seal a mac release)", () => withRepo((root, releaseId) => {
  writeHardenedReceipt(root, { releaseId, receiptPlatform: "win", platform: "mac" });
  refusedBoth(root, releaseId, /receipt is for platform win, this is mac/);
}));

test("wrong release id is refused", () => withRepo((root, releaseId) => {
  writeHardenedReceipt(root, { releaseId, receiptReleaseId: "demo-1.0.0-deadbeef" });
  refusedBoth(root, releaseId, /receipt is for release demo-1\.0\.0-deadbeef/);
}));

test("an unapproved or unlisted runner digest is refused", () => withRepo((root, releaseId) => {
  writeHardenedReceipt(root, { releaseId, runnerSha: OTHER_RUNNER_SHA, spawn: accepting });
  refusedBoth(root, releaseId, /runner sha256 9{16} is not an approved runner/);
  // An empty approved list approves nothing.
  const lines = [];
  const empty = admitSourceQa(gateOpts(root, releaseId), { warn: (line) => lines.push(line), policy: policyWith({ approvedRunners: [] }) });
  assert.equal(empty.status, "rejected");
  assert.match(empty.reason, /no rightkit-qa runner digest is approved/);
}));

test("one skipped release-critical journey is refused in refuse mode and warned in warn mode", () => withRepo((root, releaseId) => {
  // The run side already seals a failed receipt when a critical journey is not passed.
  const { validation } = writeHardenedReceipt(root, { releaseId, statuses: { "J-002": "skipped" } });
  assert.equal(validation.ok, false);
  assert.match(validation.problems.join(";"), /release-critical journey J-002 is skipped/);
  refusedBoth(root, releaseId, /records a failed QA run/);
}));

test("the admit side recomputes the critical set: a forged block claiming a skipped journey passed is still refused", () => withRepo((root, releaseId) => {
  // Block says J-002 skipped while the sealed required item says passed: the MAC is valid, the hardening is not fooled.
  writeHardenedReceipt(root, { releaseId, tweakBlock: (block) => ({ ...block, results: block.results.map((r) => (r.id === "J-002" ? { ...r, status: "skipped" } : r)) }) });
  refusedBoth(root, releaseId, /release-critical journeys not passed on macos: J-002 \(skipped\)/);
  // A block that omits a critical journey entirely.
  writeHardenedReceipt(root, { releaseId, tweakBlock: (block) => ({ ...block, results: block.results.filter((r) => r.id !== "J-002") }) });
  refusedBoth(root, releaseId, /J-002 \(missing\)/);
}));

test("windows requires its own critical journey (J-004) as well", () => withRepo((root) => {
  const releaseId = releaseIdOf(root);
  writeHardenedReceipt(root, { releaseId, platform: "win" });
  const sealed = admitSourceQa(gateOpts(root, releaseId, "win"), { warn: noWarn, policy: policyWith({ apps: REFUSE }) });
  assert.equal(sealed.status, "passed");
  writeHardenedReceipt(root, { releaseId, platform: "win", tweakBlock: (block) => ({ ...block, results: block.results.filter((r) => r.id !== "J-004") }) });
  refusedBoth(root, releaseId, /J-004 \(missing\)/, "win");
}));

test("signing outside test execution: overlap with the run, a test pid and a QA-run environment are refused", () => withRepo((root, releaseId) => {
  const now = Date.now();
  const iso = (offset) => new Date(now + offset * 1000).toISOString();
  // Sealed before the test run finished.
  writeHardenedReceipt(root, { releaseId, nowMs: now, execution: { startedAt: iso(-120), finishedAt: iso(-2), runnerPids: [4242], sealerPid: 4343 } });
  refusedBoth(root, releaseId, /sealed before the test run finished/);
  // Signing began before the run finished.
  writeHardenedReceipt(root, { releaseId, nowMs: now });
  assert.throws(() => admitSourceQa(gateOpts(root, releaseId), { warn: noWarn, policy: policyWith({ apps: REFUSE }), signingStartedAt: iso(-60) }), /signing began before the test run finished/);
  // The sealing process was a test process.
  writeHardenedReceipt(root, { releaseId, nowMs: now, execution: { startedAt: iso(-120), finishedAt: iso(-10), runnerPids: [4242], sealerPid: 4242 } });
  refusedBoth(root, releaseId, /sealing process is a test process/);
  // The admitting process is a test process.
  writeHardenedReceipt(root, { releaseId, nowMs: now });
  assert.throws(() => admitSourceQa(gateOpts(root, releaseId), { warn: noWarn, policy: policyWith({ apps: REFUSE }), pid: 4242 }), /admission is running inside a test process/);
  // Admission invoked from inside a QA run.
  assert.throws(() => admitSourceQa(gateOpts(root, releaseId, "mac", { env: { RIGHT_RELEASE_QA_STAGE: "source" } }), { warn: noWarn, policy: policyWith({ apps: REFUSE }) }), /RIGHT_RELEASE_QA_STAGE is set/);
  // No execution record at all.
  writeHardenedReceipt(root, { releaseId, nowMs: now, execution: false });
  refusedBoth(root, releaseId, /records no execution window/);
}));

test("right-release qa refuses to seal from inside a QA run", async () => {
  await assert.rejects(runQa({ stage: "source", platform: "mac", configPath: path.join(tmpdir(), "x", "right-release.config.mjs"), deps: { env: { RIGHT_RELEASE_QA_STAGE: "source" } } }), /refusing to seal a QA receipt inside a QA run/);
});

test("installed stage is hardened the same way (platform mismatch refused in refuse mode)", () => withRepo((root, releaseId) => {
  writeHardenedReceipt(root, { stage: "installed", releaseId, platform: "mac", receiptPlatform: "win", artifacts: [{ name: "demo.dmg", sha256: SHA("a"), sizeBytes: 3 }], sourceReceiptDigest: SHA("c") });
  const manifest = { releaseId, app: "demo", commit: sourceIdentity(root).commit, files: [{ name: "demo.dmg", sha256: SHA("a"), sizeBytes: 3 }], qa: { sourceReceiptDigest: SHA("c") } };
  assert.throws(() => admitInstalledQa({ ...gateOpts(root, releaseId), manifest }, { provider: "r2", warn: noWarn, policy: policyWith({ apps: REFUSE }) }), QaAdmissionError);
}));

test("validateJourneysReceipt: validator refusal, wrong host, wrong manifest, zero critical journeys", () => withRepo((root) => {
  const dir = mkdtempSync(path.join(tmpdir(), "rk-qa-jr-"));
  const manifestFile = path.join(root, "journeys.toml");
  const base = { runnerFile: "rightkit-qa", runnerSha256: RUNNER_SHA, manifestFile, manifestRelPath: "journeys.toml", platform: "mac" };
  const write = (json) => { const file = path.join(dir, "r.json"); writeFileSync(file, JSON.stringify(json)); return file; };
  const good = journeysReceiptJson({ root, startedAt: "2026-10-24T00:00:00Z", endedAt: "2026-10-24T00:01:00Z" });
  assert.equal(validateJourneysReceipt({ ...base, receiptFile: write(good), spawn: accepting }).ok, true);
  assert.match(validateJourneysReceipt({ ...base, receiptFile: write(good), spawn: refusing }).problems.join(";"), /validate-receipt refused the receipt \(exit 1\)/);
  // exit 0 without the ACCEPTED line is not accepted.
  assert.equal(validateJourneysReceipt({ ...base, receiptFile: write(good), spawn: () => ({ status: 0, stdout: "ok\n" }) }).ok, false);
  assert.match(validateJourneysReceipt({ ...base, receiptFile: write({ ...good, host: { ...good.host, os: "windows" } }), spawn: accepting }).problems.join(";"), /ran on windows, this release is macos/);
  assert.match(validateJourneysReceipt({ ...base, receiptFile: write({ ...good, manifest: { ...good.manifest, sha256: SHA("1") } }), spawn: accepting }).problems.join(";"), /different journeys manifest/);
  assert.match(validateJourneysReceipt({ ...base, receiptFile: write({ ...good, source: { ...good.source, dirty: true } }), spawn: accepting }).problems.join(";"), /dirty source tree/);
  assert.match(validateJourneysReceipt({ ...base, receiptFile: path.join(dir, "missing.json"), spawn: accepting }).problems.join(";"), /unreadable/);
  const noCritical = path.join(dir, "j.toml");
  writeFileSync(noCritical, 'version = 1\napp = "demo"\n[[journey]]\nid = "J-9"\nplatforms = ["macos"]\nrelease_critical = false\n');
  const none = validateJourneysReceipt({ ...base, manifestFile: noCritical, receiptFile: write({ ...good, manifest: { ...good.manifest, sha256: SHA("1") } }), spawn: accepting });
  assert.match(none.problems.join(";"), /no release-critical journey for macos/);
  rmSync(dir, { recursive: true, force: true });
}));

test("criticalJourneyIds reads the manifest per platform; hardenQaReceipt is exported for callers that hold a receipt", () => {
  assert.deepEqual(criticalJourneyIds(MANIFEST_TOML, "mac"), ["J-001", "J-002"]);
  assert.deepEqual(criticalJourneyIds(MANIFEST_TOML, "win"), ["J-001", "J-002", "J-004"]);
  assert.equal(typeof hardenQaReceipt, "function");
});

test("the floor maps `journey` to all release-critical journeys passed", () => {
  const { floor } = loadQaFloor(here);
  assert.deepEqual(validateQaFloor(floor), []);
  assert.deepEqual(floor.journeys, { floorId: "journey", receiptSchema: "rightkit.journeys.receipt/1", rule: "all-release-critical-passed" });
  assert.match(validateQaFloor({ ...floor, journeys: { ...floor.journeys, rule: "one-matching-check" } }).join(";"), /all-release-critical-passed/);
  assert.match(validateQaFloor({ ...floor, journeys: { ...floor.journeys, floorId: "other" } }).join(";"), /floorId/);
  // With qa.journeys the evidence-check mapping for `journey` is not required; without it it still is.
  const base = { version: "1", config: "rightkit-qa.toml", checks: { "smoke.launch": ["app.launch"] } };
  assert.deepEqual(validateAppQaConfig({ ...base, journeys: {} }, floor, "source"), []);
  assert.match(validateAppQaConfig(base, floor, "source").join(";"), /journey/);
});

test("policy: default is warn, refuse is recorded per app, malformed or missing files are hard errors", () => {
  const dir = mkdtempSync(path.join(tmpdir(), "rk-qa-policy-"));
  try {
    cpSync(path.join(here, QA_ADMISSION_POLICY_FILE), path.join(dir, QA_ADMISSION_POLICY_FILE));
    const shipped = loadAdmissionPolicy({ dir, baseline: false }).policy;
    assert.deepEqual(validateAdmissionPolicy(shipped), []);
    assert.deepEqual(shipped.apps, {}, "the shipped policy starts with every app on warn");
    const entry = setAdmissionMode({ app: "demo", mode: "refuse", setBy: "automation", dir, now: () => new Date("2026-10-24T10:00:00Z") });
    assert.deepEqual(entry, { mode: "refuse", since: "2026-10-24", set_by: "automation" });
    assert.equal(loadAdmissionPolicy({ dir, baseline: false }).policy.apps.demo.mode, "refuse");
    writeFileSync(path.join(dir, QA_ADMISSION_POLICY_FILE), "{not json");
    assert.throws(() => loadAdmissionPolicy({ dir, baseline: false }), QaPolicyError);
    writeFileSync(path.join(dir, QA_ADMISSION_POLICY_FILE), JSON.stringify({ schema: 1, apps: { demo: { mode: "maybe", since: "x", set_by: "" } }, approvedRunners: [{ sha256: "nope" }] }));
    assert.throws(() => loadAdmissionPolicy({ dir, baseline: false }), /mode must be warn or refuse.*since must be YYYY-MM-DD.*set_by is required.*sha256/);
    rmSync(path.join(dir, QA_ADMISSION_POLICY_FILE));
    assert.throws(() => loadAdmissionPolicy({ dir, baseline: false }), /policy is missing/);
  } finally { rmSync(dir, { recursive: true, force: true }); }
});

test("policy: refuse -> warn by a non-owner is refused, by adrian is allowed", () => {
  const dir = mkdtempSync(path.join(tmpdir(), "rk-qa-policy-"));
  try {
    cpSync(path.join(here, QA_ADMISSION_POLICY_FILE), path.join(dir, QA_ADMISSION_POLICY_FILE));
    setAdmissionMode({ app: "demo", mode: "refuse", setBy: "automation", dir });
    for (const setBy of ["automation", "Adrian", "someone-else"]) {
      assert.throws(() => setAdmissionMode({ app: "demo", mode: "warn", setBy, dir }), /refuse -> warn requires set_by "adrian"/);
    }
    assert.equal(loadAdmissionPolicy({ dir, baseline: false }).policy.apps.demo.mode, "refuse", "a refused downgrade leaves the file untouched");
    assert.equal(setAdmissionMode({ app: "demo", mode: "warn", setBy: "adrian", dir }).mode, "warn");
    // warn -> refuse needs no owner.
    assert.equal(setAdmissionMode({ app: "demo", mode: "refuse", setBy: "automation", dir }).mode, "refuse");
  } finally { rmSync(dir, { recursive: true, force: true }); }
});

test("policy: a hand-edited downgrade is caught against the baseline, an owner downgrade is accepted", () => {
  const dir = mkdtempSync(path.join(tmpdir(), "rk-qa-policy-"));
  try {
    const refuse = { schema: 1, apps: { demo: { mode: "refuse", since: "2026-10-24", set_by: "automation" } }, approvedRunners: [] };
    const baseline = JSON.stringify(refuse);
    const write = (apps) => writeFileSync(path.join(dir, QA_ADMISSION_POLICY_FILE), JSON.stringify({ ...refuse, apps }));
    write({ demo: { mode: "warn", since: "2026-10-25", set_by: "automation" } });
    assert.throws(() => loadAdmissionPolicy({ dir, baseline }), /weakens its baseline.*refuse -> warn requires set_by "adrian"/);
    write({});
    assert.throws(() => loadAdmissionPolicy({ dir, baseline }), /refuse entry was removed/);
    write({ demo: { mode: "warn", since: "2026-10-25", set_by: "adrian" } });
    assert.equal(loadAdmissionPolicy({ dir, baseline }).policy.apps.demo.mode, "warn");
    write({ demo: { mode: "refuse", since: "2026-10-25", set_by: "automation" } });
    assert.equal(loadAdmissionPolicy({ dir, baseline }).baseline, "supplied");
    assert.deepEqual(checkPolicyTransition({ apps: { a: { mode: "warn" } } }, { apps: {} }), [], "removing a warn entry is not a downgrade");
  } finally { rmSync(dir, { recursive: true, force: true }); }
});

test("policy errors from the admit wrappers are never swallowed as warnings", () => withRepo((root, releaseId) => {
  const dir = mkdtempSync(path.join(tmpdir(), "rk-qa-policy-"));
  try {
    writeFileSync(path.join(dir, QA_ADMISSION_POLICY_FILE), "{not json");
    assert.throws(() => admitSourceQa(gateOpts(root, releaseId), { warn: noWarn, policyDir: dir }), QaPolicyError);
  } finally { rmSync(dir, { recursive: true, force: true }); }
}));

test("an app absent from the policy is warn; the missing-receipt refusal is a throw only in refuse mode", () => withRepo((root, releaseId) => {
  const lines = [];
  const missing = admitSourceQa(gateOpts(root, releaseId), { warn: (line) => lines.push(line), policy: policyWith() });
  assert.equal(missing.status, "missing");
  assert.equal(lines.length, 1);
  assert.throws(() => admitSourceQa(gateOpts(root, releaseId), { warn: noWarn, policy: policyWith({ apps: REFUSE }) }), /QA admission refused for demo .*no source QA receipt/s);
  assert.equal(appOfReleaseId("demo-1.0.0-abcd1234"), "demo");
  assert.equal(appOfReleaseId("my-app-2.10.3-beta.1-abcd1234"), "my-app");
}));

test("the shipped policy file and runner approvals are plain data in the package", () => {
  const shipped = JSON.parse(readFileSync(path.join(here, QA_ADMISSION_POLICY_FILE), "utf8"));
  assert.deepEqual(validateAdmissionPolicy(shipped), []);
});
