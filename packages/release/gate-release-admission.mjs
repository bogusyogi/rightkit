// Admission of a gate-package release (docs/GATE-CONTRACT.md NN31: no gate is released without a passing contract test; loop matrix G21, G26).
//
// Run where the package archive is about to be published. It has no broker and is not the host that ran the test, so it judges only what it is
// handed: the host's contract-test receipt, the folder of signed job receipts beside it, the archive in question, and this checkout at the
// release commit. It refuses unless every one of the following holds.
//   1. The receipt passes the release check (gate-acceptance.mjs verifyReleaseReceipts) for this commit, with the contract, budgets and runner
//      files of this checkout, the exact rule set, and names exactly the archive in question. It is a candidate's receipt.
//   2. The host is one this repository trusts: ci/rk-pipeline.json attestation.<platform> gives its key, host name and broker build. The receipt's
//      broker is that build (loop matrix C9: a host whose installed broker is not the committed one cannot be admitted until that list is updated).
//   3. The run's broker jobs: the list is not empty; every listed job's receipt is in the bundle with the listed hash, carries the broker's
//      signature for that key, host and broker build, is for that job id, and its signed nonce is the one computed from the run id, the sealed gate,
//      the step and the call (gate-seam.mjs contractJobNonce); and every step this checkout's contract-nn.json names in `signedSteps` (the steps
//      that cannot be cache hits) has at least one such job.
// What this proves, and no more (rustright attested-runs.md section 5): real jobs of this run, for this gate, went through that host's broker.
// The key is readable by the account the broker runs as, so this is not a defence against a hostile program on that host. The receipt's verdict
// itself is the runner's own judgement (`nnBasis`), and the result says so.
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { createHash } from "node:crypto";
import { BUDGETS_FILE, CONTRACT_FILE, RUNNER_FILE, archiveIdentity, runnerFileHashes, validWaiver, verifyReleaseReceipts } from "./gate-acceptance.mjs";
import { contractJobNonce } from "./gate-seam.mjs";
import { verifyProvenanceSignature } from "./receipt-signature.mjs";

const sha256 = (bytes) => createHash("sha256").update(bytes).digest("hex");
const PLATFORM_OF = Object.freeze({ mac: "darwin", dell: "win32" });
const stepIdOf = (label) => String(label).split(":")[0];

/**
 * `receipt`: the contract-test receipt (parsed). `readJob(requestId)`: the bytes of that job's receipt from the bundle (throws when absent).
 * `archiveBytes`: the package archive in question. `commit`: the release commit. `pipeline`: ci/rk-pipeline.json (parsed). `sealed`: this
 * checkout's contract-nn.json (parsed). `contractSha256`, `budgetsSha256`, `runnerSha256`, `runnerFiles`: of this checkout. `waivers`: valid waivers.
 * Returns { ok, problems, facts }.
 */
export function admitGateRelease({ receipt, readJob, archiveBytes, commit, pipeline, sealed, contractSha256, budgetsSha256, runnerSha256, runnerFiles, waivers = [] }) {
  const problems = [];
  const facts = { host: receipt?.host ?? null, sealedSha256: receipt?.gate?.sealedSha256 ?? null, archiveSha256: null, jobsListed: 0, jobsVerified: 0, signedSteps: [], nnBasis: receipt?.nnBasis ?? null };
  const platform = PLATFORM_OF[receipt?.host];
  const pins = platform ? pipeline?.attestation?.[platform] : null;
  if (!platform) problems.push(`the receipt is from ${JSON.stringify(receipt?.host)}, not a host this repository names`);
  else if (!pins || typeof pins.publicKey !== "string" || typeof pins.keyId !== "string" || typeof pins.host !== "string" || typeof pins.brokerDigest !== "string") problems.push(`ci/rk-pipeline.json has no complete attestation.${platform} (publicKey, keyId, host, brokerDigest)`);
  let archive = null;
  try { archive = archiveIdentity(archiveBytes); facts.archiveSha256 = archive.sha256; } catch (e) { problems.push(`the archive in question cannot be hashed (${e.message})`); }
  if (receipt?.gate?.kind !== "candidate") problems.push(`the receipt is a ${JSON.stringify(receipt?.gate?.kind)} gate's, not a candidate's: only a candidate's receipt names the archive it was installed from`);
  // 1 and the broker half of 2: the release check, with everything this checkout and the committed list know.
  problems.push(...verifyReleaseReceipts(receipt, null, { commit, contractSha256, runnerSha256, budgetsSha256, runnerFiles, waivers, sealedSha256: receipt?.gate?.sealedSha256 ?? null, brokerRuntimeDigest: pins?.brokerDigest ?? null, archive: archive ?? { sha256: null, integrity: null } }).problems);
  if (!commit) problems.push("no release commit was given");
  // 3: the run's broker jobs.
  const jobs = Array.isArray(receipt?.jobs) ? receipt.jobs : null;
  if (typeof receipt?.runId !== "string" || !/^[0-9a-f]{64}$/.test(receipt.runId)) problems.push("the receipt carries no run id, so its jobs cannot be tied to it");
  if (!jobs) problems.push("the receipt lists no broker jobs (jobs is missing)");
  else if (jobs.length === 0) problems.push("the receipt lists no broker job at all: nothing in it is signed");
  const verifiedSteps = new Set();
  for (const [i, job] of (jobs ?? []).entries()) {
    facts.jobsListed += 1;
    const name = `job ${job?.requestId ?? `#${i + 1}`} (${job?.step})`;
    if (typeof job?.requestId !== "string" || !job.requestId) continue; // refused before it existed: proves nothing, counts for nothing
    if (!/^[A-Za-z0-9._-]{1,128}$/.test(job.requestId)) { problems.push(`${name}: its job id cannot name a file`); continue; }
    let bytes; try { bytes = readJob(job.requestId); } catch (e) { problems.push(`${name}: its signed receipt is not in the bundle (${e.code ?? e.message})`); continue; }
    if (sha256(bytes) !== job.receiptSha256) { problems.push(`${name}: the receipt in the bundle is not the one listed (sha256 differs)`); continue; }
    let jobReceipt; try { jobReceipt = JSON.parse(String(bytes)); } catch { problems.push(`${name}: its receipt is not JSON`); continue; }
    let payload;
    try { payload = verifyProvenanceSignature(jobReceipt, pins, job.requestId); } catch (e) { problems.push(`${name}: ${e.code ? `${e.code}: ` : ""}${e.message}`); continue; }
    let want = null; try { want = contractJobNonce({ runId: receipt.runId, sealedSha256: receipt.gate?.sealedSha256, step: job.step, call: job.call }); } catch { /* reported next */ }
    if (want === null || payload.nonce !== want) { problems.push(`${name}: the nonce the broker signed is not this run's for this gate, step and call`); continue; }
    facts.jobsVerified += 1; verifiedSteps.add(stepIdOf(job.step));
  }
  facts.signedSteps = [...verifiedSteps].sort();
  const required = Array.isArray(sealed?.signedSteps) ? sealed.signedSteps : null;
  if (!required || required.length === 0) problems.push("this checkout's contract-nn.json names no steps that must show signed jobs (signedSteps)");
  else for (const id of required) if (!verifiedSteps.has(id)) problems.push(`step ${id} cannot be served from a cache and shows no verified broker job`);
  return { ok: problems.length === 0, problems, facts };
}

/** `node gate-release-admission.mjs --receipt <file> --archive <package archive> --commit <sha> [--pipeline <ci/rk-pipeline.json>]`: exit 0 only when admitted. */
export function main(argv = process.argv.slice(2), { out = console.log } = {}) {
  const opt = (name) => { const i = argv.indexOf(name); return i < 0 ? undefined : argv[i + 1]; };
  const known = new Set(["--receipt", "--archive", "--commit", "--pipeline"]);
  for (const a of argv) if (a.startsWith("--") && !known.has(a)) { out(`gate-release-admission REFUSED: unknown option ${a}`); return 64; }
  const receiptFile = opt("--receipt"); const archiveFile = opt("--archive"); const commit = opt("--commit");
  const here = path.dirname(fileURLToPath(import.meta.url));
  const pipelineFile = opt("--pipeline") ?? path.resolve(here, "..", "..", "ci", "rk-pipeline.json");
  const problems = [];
  if (!receiptFile) problems.push("--receipt <contract-test receipt> is required");
  if (!archiveFile) problems.push("--archive <package archive in question> is required");
  if (!commit || !/^[0-9a-f]{40}$/.test(commit)) problems.push("--commit <40-hex release commit> is required");
  const read = (label, f, parse) => { try { const b = fs.readFileSync(f); return parse ? JSON.parse(String(b)) : b; } catch (e) { problems.push(`${label} ${f} cannot be read (${e.code ?? e.message})`); return null; } };
  const receipt = receiptFile ? read("the receipt", receiptFile, true) : null;
  const archiveBytes = archiveFile ? read("the archive", archiveFile, false) : null;
  const pipeline = read("the pipeline file", pipelineFile, true);
  const sealed = read("contract-nn.json", path.join(here, "contract-nn.json"), true);
  const budgets = read("the budgets file", BUDGETS_FILE, true);
  let result = null;
  if (!problems.length) {
    const folder = receipt.jobReceipts;
    if (folder !== "job-receipts") problems.push(`the receipt names its job receipts folder as ${JSON.stringify(folder)}, expected "job-receipts"`);
    else {
      const bundle = path.join(path.dirname(path.resolve(receiptFile)), "job-receipts");
      const fileSha = (f) => sha256(fs.readFileSync(f));
      result = admitGateRelease({ receipt, readJob: (id) => fs.readFileSync(path.join(bundle, `${id}.json`)), archiveBytes, commit, pipeline, sealed, contractSha256: fileSha(CONTRACT_FILE), budgetsSha256: fileSha(BUDGETS_FILE), runnerSha256: fileSha(RUNNER_FILE), runnerFiles: runnerFileHashes(), waivers: (Array.isArray(budgets?.waivers) ? budgets.waivers : []).filter(validWaiver) });
      problems.push(...result.problems);
    }
  }
  if (problems.length) { out(`gate-release-admission REFUSED (${problems.length} problem(s))`); for (const p of problems) out(`  ${p}`); return 1; }
  const f = result.facts;
  out(`gate-release-admission OK: host ${f.host}, sealed gate ${f.sealedSha256}, archive ${f.archiveSha256}; ${f.jobsVerified} of ${f.jobsListed} listed broker job(s) verified, in step(s) ${f.signedSteps.join(", ")}`);
  out(`  the verdict itself rests on: ${f.nnBasis ?? "not stated"} (signed jobs prove the run happened on that host's broker for this gate; they do not re-derive the verdict)`);
  return 0;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) process.exit(main());
