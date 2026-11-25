// Admission of a gate-package release judged by the twelve-line result (docs/GATE-CONTRACT.md GC-A27, rightkit#1514 step 9; design table
// D:/scratch/loop-review/twelve-line-step9/design-table.md). Run where the archive is about to be published, with no broker: it judges only what it is handed.
// It refuses unless every one of the following holds (default refuse).
//   1. The record is a rightkit.acceptance-result/1 (the judge's --result file).
//   2. Its lines are exactly the checkout's GC-A27 table: the twelve ids, in order, with the sentences character for character.
//   3. Every verdict is "pass" and `pass` says so.
//   4. The files that judged the run (acceptance-lines.mjs, its test, acceptance-launch.mjs) hash to this checkout's.
//   5. The contract text is this checkout's.
//   6. It is for the release commit and that commit's tree.
//   7. The host is one the committed pipeline file pins (key, key id, host, broker digest).
//   8. The run was of the archive's gate: the sealed gate's manifest and source record hash to gate.sealedSha256 and name exactly the archive in question.
//   9. The broker's claims: the list is not empty; each claim is in the bundle with the listed hash, is only { id, publication }, carries the pinned broker's
//      signature for that host and build, is for that job id, and names the result's commit and tree.
// What this proves, and no more: real jobs of this commit and tree went through that host's broker, and the judge, as this checkout has it, found every line
// true from that run's own records. A publication claim carries no time and the gate's nonces in a twelve-line run are not derived from the run, so a claim is
// bound to the result by its signed commit and tree, not by the run. The verdict is the judge's own judgement and the result says so (nnBasis).
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { createHash } from "node:crypto";
import { archiveIdentity, archiveOfSeal } from "./legacy-contract-receipt.mjs";
import { readAcceptanceLines } from "./acceptance-lines.mjs";
import { verifyPublicationClaim } from "./receipt-signature.mjs";

export const ACCEPTANCE_RESULT_SCHEMA = "rightkit.acceptance-result/1";
/** The files the twelve-line judgement is made of, relative to the package. */
// Step F, the lock (Adrian, 2026-11-25): every file the acceptance judge imports (acceptance-run.mjs and everything it reaches inside this package), the cases
// file it judges against, and the old judge's test. A file the judge comes to import that is not listed here fails acceptance-admission.test.mjs.
export const JUDGE_FILES = Object.freeze(["acceptance-callers.mjs","acceptance-cases.json","acceptance-cases.mjs","acceptance-launch.mjs","acceptance-lines.mjs","acceptance-lines.test.mjs","acceptance-rows.mjs","acceptance-run.mjs","atomic-write.mjs","background-process.mjs","broker-client.mjs","disk-policy.mjs","gate-seam.mjs","gate-serve.mjs","gate-stages.mjs","managed-root-guard.mjs","managed-roots.mjs","platform-dir.mjs","rustright-names.mjs","snapshot-changes.mjs","snapshot-git.mjs","snapshot-identity.mjs","snapshot-root-config.mjs","snapshot.mjs"]);
const here = path.dirname(fileURLToPath(import.meta.url));
const sha256 = (bytes) => createHash("sha256").update(bytes).digest("hex");
const PLATFORM_OF = Object.freeze({ mac: "darwin", dell: "win32" });

/** The sha256 of each judge file in `dir` (this package by default); a file that cannot be read is null. */
export function judgeFileHashes(dir = here) {
  return Object.fromEntries(JUDGE_FILES.map((name) => { try { return [name, sha256(fs.readFileSync(path.join(dir, name)))]; } catch { return [name, null]; } }));
}

/**
 * `result`: the parsed acceptance result. `readJob(requestId)`: the bytes of that job's claim from the bundle (throws when absent). `archiveBytes`: the archive in
 * question. `commit`, `tree`: the release commit and its tree. `pipeline`: ci/rk-pipeline.json (parsed). `contractText`, `contractSha256`: of this checkout.
 * `judgeFiles`: judgeFileHashes() of this checkout. `manifestBytes`, `sourceBytes`: the sealed gate's manifest and source record. Returns { ok, problems, facts }.
 */
export function admitAcceptanceResult({ result, readJob, archiveBytes, commit, tree, pipeline, contractText, contractSha256, judgeFiles, manifestBytes = null, sourceBytes = null }) {
  const problems = [];
  const facts = { host: result?.host ?? null, version: null, sealedSha256: result?.gate?.sealedSha256 ?? null, archiveSha256: null, jobsListed: 0, jobsVerified: 0, signedSteps: [], nnBasis: "twelve-line-judge" };
  // 1
  if (result?.schema !== ACCEPTANCE_RESULT_SCHEMA) { problems.push(`the record's schema is ${JSON.stringify(result?.schema ?? null)}, not ${ACCEPTANCE_RESULT_SCHEMA}`); return { ok: false, problems, facts }; }
  // 2 and 3: the twelve lines as the checkout's table has them, every one a pass
  let want = []; try { want = readAcceptanceLines(contractText); } catch (e) { problems.push(`this checkout's GC-A27 table cannot be read (${e.message})`); }
  const lines = Array.isArray(result.lines) ? result.lines : [];
  if (want.length && (lines.length !== want.length || want.some((w, i) => lines[i]?.id !== w.id))) problems.push(`the result does not carry the twelve lines ${want.map((w) => w.id).join(", ")} in that order (it carries ${lines.map((l) => l?.id).join(", ") || "none"})`);
  else for (const [i, w] of want.entries()) {
    if (lines[i].text !== w.text) problems.push(`${w.id}: the sentence in the result is not this checkout's text`);
    if (lines[i].verdict !== "pass") problems.push(`${w.id} is ${lines[i].verdict}, not "pass"`);
  }
  if (result.pass !== true) problems.push(`the result's pass is ${JSON.stringify(result.pass ?? null)}, not true`);
  // 4 and 5: judged by this checkout's judge, against this checkout's contract
  for (const name of JUDGE_FILES) {
    const have = result.judgeFiles?.[name]; const mine = judgeFiles?.[name] ?? null;
    if (typeof have !== "string" || mine === null || have !== mine) problems.push(`judge file ${name} is ${have ?? "absent"} in the result, this checkout's is ${mine ?? "absent"}`);
  }
  if (result.contractSha256 !== contractSha256) problems.push(`the contract the result was judged against (${String(result.contractSha256).slice(0, 12)}) is not this checkout's (${String(contractSha256).slice(0, 12)})`);
  // 6
  if (!commit || !/^[0-9a-f]{40}$/.test(commit)) problems.push("no release commit was given");
  else if (result.commit !== commit) problems.push(`the result is for commit ${String(result.commit).slice(0, 12)}, the release commit is ${commit.slice(0, 12)}`);
  if (!tree) problems.push("no tree for the release commit was given");
  else if (result.tree !== tree) problems.push(`the result's tree ${String(result.tree).slice(0, 12)} is not the release commit's tree ${tree.slice(0, 12)}`);
  // 7
  const platform = PLATFORM_OF[result.host];
  const pins = platform ? pipeline?.attestation?.[platform] : null;
  if (!platform) problems.push(`the result is from ${JSON.stringify(result.host ?? null)}, not a host this repository names`);
  else if (!pins || typeof pins.publicKey !== "string" || typeof pins.keyId !== "string" || typeof pins.host !== "string" || typeof pins.brokerDigest !== "string") problems.push(`ci/rk-pipeline.json has no complete attestation.${platform} (publicKey, keyId, host, brokerDigest)`);
  // 8: the run was of the archive's gate
  let archive = null;
  try { archive = archiveIdentity(archiveBytes); facts.archiveSha256 = archive.sha256; } catch (e) { problems.push(`the archive in question cannot be hashed (${e.message})`); }
  if (!manifestBytes || !sourceBytes) problems.push("the sealed gate's manifest and source record were not handed over, so nothing ties the archive to the run's gate");
  else {
    const linked = archiveOfSeal({ sealedSha256: result.gate?.sealedSha256, manifestBytes, sourceBytes });
    for (const p of linked.problems) problems.push(`seal to archive: ${p}`);
    if (linked.ok) {
      facts.version = linked.archive.version ?? null;
      if (archive && (linked.archive.sha256 !== archive.sha256 || linked.archive.integrity !== archive.integrity)) problems.push(`seal to archive: the tested gate was installed from the archive ${linked.archive.sha256}, the archive in question is ${archive.sha256}`);
    }
  }
  if (typeof result.gate?.sealedSha256 !== "string" || !/^[0-9a-f]{64}$/.test(result.gate.sealedSha256)) problems.push("the result names no sealed gate (gate.sealedSha256), so the run is not tied to an installed gate");
  // 9: the broker's claims
  const jobs = Array.isArray(result.jobs) ? result.jobs : null;
  if (!jobs) problems.push("the result lists no broker jobs (jobs is missing)");
  else if (jobs.length === 0) problems.push("the result lists no broker job at all: nothing in it is signed");
  for (const [i, job] of (jobs ?? []).entries()) {
    facts.jobsListed += 1;
    const name = `job ${job?.requestId ?? `#${i + 1}`}`;
    if (typeof job?.requestId !== "string" || !/^[A-Za-z0-9._-]{1,128}$/.test(job.requestId)) { problems.push(`${name}: its job id cannot name a file`); continue; }
    if (typeof job.claimSha256 !== "string") { problems.push(`${name}: it carries no claim hash`); continue; }
    let bytes; try { bytes = readJob(job.requestId); } catch (e) { problems.push(`${name}: its claim is not in the bundle (${e.code ?? e.message})`); continue; }
    if (sha256(bytes) !== job.claimSha256) { problems.push(`${name}: the claim in the bundle is not the one listed (sha256 differs)`); continue; }
    let doc; try { doc = JSON.parse(String(bytes)); } catch { problems.push(`${name}: its claim is not JSON`); continue; }
    if (Object.keys(doc ?? {}).some((k) => k !== "id" && k !== "publication")) { problems.push(`${name}: the bundle holds more than the claim (only id and publication may be handed over)`); continue; }
    let payload; try { payload = verifyPublicationClaim(doc, pins, job.requestId); } catch (e) { problems.push(`${name}: ${e.code ? `${e.code}: ` : ""}${e.message}`); continue; }
    if (payload.gate?.commit !== result.commit) { problems.push(`${name}: the broker signed commit ${String(payload.gate?.commit).slice(0, 12)}, the result is for ${String(result.commit).slice(0, 12)}`); continue; }
    if (payload.gate?.tree !== result.tree) { problems.push(`${name}: the broker signed tree ${String(payload.gate?.tree).slice(0, 12)}, the result is for ${String(result.tree).slice(0, 12)}`); continue; }
    facts.jobsVerified += 1;
  }
  return { ok: problems.length === 0, problems, facts };
}
