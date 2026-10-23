// Release admission hardening (ADR-0001 item 10). Two halves:
//
//   run side   `validateJourneysReceipt` runs `rightkit-qa journeys validate-receipt` (the pinned runner, as a
//              subprocess; exit code and the final ACCEPTED line), cross-checks the receipt against the committed
//              journeys manifest, and returns the `journeys` block that `right-release qa` seals into the MAC'd
//              QA receipt. The floor id `journey` means "every release-critical journey required on this platform
//              passed", not "one matching check passed".
//   admit side `hardenQaReceipt` re-checks the sealed receipt at admission, independently of what the run side
//              claimed: platform and release id, the approved runner digest, the critical journey set recomputed
//              from the manifest committed at the receipt's commit, and the signing/test-execution separation.
//
// Signing outside test execution, what is checked here: the receipt records the pids of the test processes it
// spawned and the window in which they ran; admission refuses a receipt whose window does not end before the
// receipt was sealed and before admission (so sealing or signing cannot overlap the run), refuses when the
// admitting process is one of those test processes, refuses when RIGHT_RELEASE_QA_STAGE is set (admission or
// signing invoked from inside a QA run), and `right-release qa` refuses to seal inside a QA run. Not checkable
// here: that the test processes cannot reach the signing key (that is the OS credential store and the signing
// agent's ACL) and that CI runs QA and signing as separate jobs (the carried receipt, rightkit#183, is the hook).
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import path from "node:path";
import { parse as parseToml } from "smol-toml";
import { spawnSync } from "./background-process.mjs";
import { approvedRunnerDigests } from "./release-qa-policy.mjs";
import { QaAdmissionError, qaCommandFor } from "./release-qa-receipt.mjs";

export const JOURNEYS_RECEIPT_SCHEMA = "rightkit.journeys.receipt/1";
export const PLATFORM_OS = Object.freeze({ mac: "macos", win: "windows" });
export const QA_RUN_ENV = "RIGHT_RELEASE_QA_STAGE";
const CLOCK_SKEW_MS = 60_000;
const HEX64 = /^[a-f0-9]{64}$/;

const sha256Of = (bytes) => createHash("sha256").update(bytes).digest("hex");

/** Release-critical journey ids the manifest requires on `platform` (mac|win), sorted. */
export function criticalJourneyIds(manifestText, platform) {
  const os = PLATFORM_OS[platform];
  if (!os) throw new Error(`unknown platform ${platform}`);
  const journeys = parseToml(manifestText).journey;
  if (!Array.isArray(journeys)) return [];
  return journeys.filter((journey) => journey?.release_critical === true && Array.isArray(journey.platforms) && journey.platforms.includes(os))
    .map((journey) => String(journey.id)).sort();
}

/**
 * Validate a journeys receipt through the runner and against the manifest. Never throws for a refusal.
 * Returns { ok, problems, block }; `block` is the `journeys` section for the QA receipt (null when nothing readable).
 */
export function validateJourneysReceipt({ runnerFile, runnerSha256, receiptFile, manifestFile, manifestRelPath, platform, spawn = spawnSync }) {
  const problems = [];
  let bytes, receipt, manifestBytes;
  try { bytes = readFileSync(receiptFile); receipt = JSON.parse(bytes.toString("utf8")); }
  catch (error) { return { ok: false, problems: [`journeys receipt is unreadable (${receiptFile}): ${error.message}`], block: null }; }
  try { manifestBytes = readFileSync(manifestFile); }
  catch (error) { return { ok: false, problems: [`journeys manifest is unreadable (${manifestFile}): ${error.message}`], block: null }; }
  const os = PLATFORM_OS[platform];
  if (receipt.schema !== JOURNEYS_RECEIPT_SCHEMA) problems.push(`journeys receipt schema is ${JSON.stringify(receipt.schema)}, expected ${JOURNEYS_RECEIPT_SCHEMA}`);
  if (receipt.runner !== "rightkit-qa") problems.push("journeys receipt was not produced by the rightkit-qa runner");
  if (receipt.host?.os !== os) problems.push(`journeys receipt ran on ${receipt.host?.os}, this release is ${os}`);
  if (receipt.selection?.platform !== os) problems.push(`journeys receipt selected platform ${receipt.selection?.platform}, this release is ${os}`);
  if (receipt.manifest?.sha256 !== sha256Of(manifestBytes)) problems.push("journeys receipt was run against a different journeys manifest than the committed one");
  if (!/^[a-f0-9]{40}$/.test(receipt.source?.tree ?? "")) problems.push("journeys receipt source.tree is not a 40-hex tree");
  if (receipt.source?.dirty !== false) problems.push("journeys receipt records a dirty source tree");

  // The runner re-checks every D5 rule; the exit code and the final line must both say accepted.
  const run = spawn(runnerFile, ["journeys", "validate-receipt", receiptFile, "--manifest", manifestFile, "--tree", String(receipt.source?.tree ?? "")], { encoding: "utf8", windowsHide: true, maxBuffer: 16 * 1024 * 1024 });
  const lines = String(run?.stdout ?? "").split(/\r?\n/).filter(Boolean);
  const accepted = run?.status === 0 && /: ACCEPTED$/.test(lines.at(-1) ?? "");
  if (!accepted) problems.push(`rightkit-qa journeys validate-receipt refused the receipt (exit ${run?.status ?? "none"}): ${lines.slice(0, 5).join(" | ") || String(run?.stderr ?? "").trim() || "no output"}`);

  // Every release-critical journey required on this platform, from the manifest, must be passed in the receipt.
  let required = [];
  try { required = criticalJourneyIds(manifestBytes.toString("utf8"), platform); }
  catch (error) { problems.push(`journeys manifest cannot be parsed: ${error.message}`); }
  if (!required.length) problems.push(`the journeys manifest declares no release-critical journey for ${os}; zero is never a pass`);
  const byId = new Map((receipt.journeys ?? []).map((journey) => [journey.id, journey]));
  for (const id of required) {
    const journey = byId.get(id);
    if (!journey) problems.push(`release-critical journey ${id} has no result in the receipt`);
    else if (journey.status !== "passed") problems.push(`release-critical journey ${id} is ${journey.status}`);
    else if (journey.release_critical !== true) problems.push(`release-critical journey ${id} is not marked release_critical in the receipt`);
  }
  const block = {
    schema: JOURNEYS_RECEIPT_SCHEMA, receiptSha256: sha256Of(bytes), runId: String(receipt.run_id ?? ""), verdict: String(receipt.verdict ?? ""),
    hostOs: String(receipt.host?.os ?? ""), selectionPlatform: String(receipt.selection?.platform ?? ""), tree: String(receipt.source?.tree ?? ""),
    startedAt: String(receipt.started_at ?? ""), endedAt: String(receipt.ended_at ?? ""), runnerVersion: String(receipt.runner_version ?? ""),
    validator: { runnerSha256, exitCode: run?.status ?? null, accepted },
    manifest: { path: manifestRelPath, sha256: sha256Of(manifestBytes) },
    required, results: (receipt.journeys ?? []).map((journey) => ({ id: String(journey.id), status: String(journey.status), releaseCritical: journey.release_critical === true })),
  };
  return { ok: problems.length === 0, problems, block };
}

/** The sealed `required` entry for the floor's journey id: passed only when every critical journey passed. */
export function journeyRequiredItem({ floorId, validation }) {
  const results = validation.block?.results ?? [];
  const status = validation.ok ? "passed" : validation.block ? "failed" : "missing";
  return { id: floorId, status, evidenceChecks: results.map((result) => ({ name: result.id, status: result.status })), ...(validation.ok ? {} : { problems: validation.problems }) };
}

const readCommitted = ({ repoRoot, commit, file, gitRunner = spawnSync }) => {
  const result = gitRunner("git", ["-C", repoRoot, "show", `${commit}:${file}`], { encoding: "utf8", windowsHide: true, maxBuffer: 64 * 1024 * 1024 });
  if (result?.status !== 0) throw new Error(`git show ${commit.slice(0, 12)}:${file} failed`);
  return String(result.stdout);
};

/**
 * Admission-time re-check of a receipt that already passed `verifyQaReceipt`. Throws QaAdmissionError.
 * `releaseId` and `platform` are what is being sealed (source) or published (installed). `signingStartedAt` is
 * optional: a caller that knows when signing began passes it; the default is `now`.
 */
export function hardenQaReceipt({
  receipt, stage, platform, releaseId, policy, floor, repoRoot, configName, gitRunner, env = process.env, now = () => new Date(), pid = process.pid, signingStartedAt,
}) {
  const command = qaCommandFor({ stage, platform, releaseId, configName });
  const refuse = (reason) => { throw new QaAdmissionError(`${stage} QA receipt rejected: ${reason}`, { command }); };

  // 1. The receipt is for this platform and this release id.
  if (receipt.platform !== platform) refuse(`receipt is for platform ${receipt.platform}, this is ${platform}`);
  if (receipt.releaseId !== releaseId) refuse(`receipt is for release ${receipt.releaseId}, this is ${releaseId}`);

  // 2. The qualifying runner is pinned by an approved digest.
  const approved = approvedRunnerDigests(policy);
  if (!approved.length) refuse("no rightkit-qa runner digest is approved in qa-admission-policy.json");
  if (!approved.includes(receipt.runner?.sha256)) refuse(`rightkit-qa runner sha256 ${String(receipt.runner?.sha256).slice(0, 16)} is not an approved runner`);

  // 3. The journey floor id means every release-critical journey required on this platform passed.
  const mapping = floor?.journeys;
  if (!mapping?.floorId) refuse("the QA floor does not map the journey floor id to a journeys receipt");
  const journeys = receipt.journeys;
  if (!journeys) refuse("receipt carries no journeys receipt block; run `rightkit-qa journeys run` through `right-release qa` (qa.journeys in the release config)");
  const os = PLATFORM_OS[platform];
  if (journeys.schema !== mapping.receiptSchema) refuse(`journeys receipt schema is ${journeys.schema}`);
  if (journeys.verdict !== "passed" || journeys.validator?.accepted !== true || journeys.validator?.exitCode !== 0) refuse("the journeys receipt was not accepted by rightkit-qa journeys validate-receipt");
  if (journeys.validator?.runnerSha256 !== receipt.runner?.sha256) refuse("the journeys receipt was validated by a different runner than the one pinned");
  if (journeys.hostOs !== os || journeys.selectionPlatform !== os) refuse(`journeys receipt is for ${journeys.hostOs}/${journeys.selectionPlatform}, this is ${os}`);
  if (journeys.tree !== receipt.tree) refuse("journeys receipt source tree differs from the QA receipt tree");
  if (!HEX64.test(journeys.receiptSha256 ?? "") || !HEX64.test(journeys.manifest?.sha256 ?? "")) refuse("journeys block has malformed digests");
  let committedManifest;
  try { committedManifest = readCommitted({ repoRoot, commit: receipt.commit, file: journeys.manifest.path, gitRunner }); }
  catch (error) { refuse(`the journeys manifest at the receipt's commit cannot be read: ${error.message}`); }
  if (sha256Of(committedManifest) !== journeys.manifest.sha256) refuse("the journeys manifest committed at the receipt's commit differs from the one the journeys run used");
  let required;
  try { required = criticalJourneyIds(committedManifest, platform); } catch (error) { refuse(`journeys manifest cannot be parsed: ${error.message}`); }
  if (!required.length) refuse(`the journeys manifest declares no release-critical journey for ${os}; zero is never a pass`);
  const byId = new Map((journeys.results ?? []).map((result) => [result.id, result]));
  const notPassed = required.filter((id) => byId.get(id)?.status !== "passed" || byId.get(id)?.releaseCritical !== true);
  if (notPassed.length) refuse(`release-critical journeys not passed on ${os}: ${notPassed.map((id) => `${id} (${byId.get(id)?.status ?? "missing"})`).join(", ")}`);
  const item = (receipt.required ?? []).find((entry) => entry.id === mapping.floorId);
  if (item?.status !== "passed") refuse(`floor id ${mapping.floorId} is ${item?.status ?? "not recorded"}`);

  // 4. Signing (and admission) happen outside test execution.
  const exec = receipt.execution;
  if (!exec) refuse("receipt records no execution window; it predates the signing/test separation rule");
  const at = (value) => Date.parse(value);
  const [started, finished, created, nowMs] = [at(exec.startedAt), at(exec.finishedAt), at(receipt.createdAt), now().getTime()];
  if ([started, finished, created].some(Number.isNaN)) refuse("execution window has malformed timestamps");
  if (!(started <= finished)) refuse("execution window ends before it starts");
  if (finished > created) refuse("the receipt was sealed before the test run finished (sealing overlapped test execution)");
  const signing = signingStartedAt ? at(signingStartedAt) : nowMs;
  if (Number.isNaN(signing) || finished > signing) refuse("signing began before the test run finished (signing overlapped test execution)");
  if (created > nowMs + CLOCK_SKEW_MS) refuse("receipt is dated in the future");
  const jStart = at(journeys.startedAt), jEnd = at(journeys.endedAt);
  if (Number.isNaN(jStart) || Number.isNaN(jEnd) || jStart < started - CLOCK_SKEW_MS || jEnd > finished + CLOCK_SKEW_MS) refuse("the journeys run is not inside the recorded execution window");
  const testPids = exec.runnerPids;
  if (!Array.isArray(testPids) || !testPids.length || testPids.some((value) => !Number.isInteger(value) || value < 1)) refuse("receipt records no test process ids");
  if (testPids.includes(exec.sealerPid)) refuse("the sealing process is a test process");
  if (testPids.includes(pid)) refuse("admission is running inside a test process");
  if (env?.[QA_RUN_ENV]) refuse(`${QA_RUN_ENV} is set: admission or signing was invoked from inside a QA run`);
  return { requiredJourneyIds: required, testPids };
}

export const relativePosix = (root, file) => path.relative(root, file).split(path.sep).join("/");
