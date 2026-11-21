// The twelve-line result (rightkit.acceptance-result/1, acceptance-admission.mjs) is admitted first; what follows describes the old contract-test receipt, still read against the
// hashes contract-nn.json keeps of the retired runner (`retiredRunner`; the runner itself was deleted in rightkit#1514 step 9).
// Admission of a gate-package release (docs/GATE-CONTRACT.md NN31: no gate is released without a passing contract test; loop matrix G21, G26).
//
// Run where the package archive is about to be published. It has no broker and is not the host that ran the test, so it judges only what it is
// handed: the host's contract-test receipt, the folder of job claims beside it, the archive in question, and this checkout at the release
// commit. A job claim is the small statement the broker signs to leave the host ({ id, publication }; rustright GB-104): the key, the host name,
// the broker build, the job id, the nonce, the commit, the result and the kind of command. The whole receipts and their full signed part name
// the environment variables the broker dropped and the host's paths; they stay on the host. This repository and its CI are public. It refuses unless every one of the following holds.
//   1. The receipt passes the release check (legacy-contract-receipt.mjs verifyReleaseReceipts) for this commit, with the contract, budgets and runner
//      files of this checkout, the exact rule set, and names exactly the archive in question. It is a candidate's receipt.
//  1b. The archive is tied to the tested gate by the seal itself, not by the receipt's statement: the manifest handed over hashes to the seal (and the
//      seal is inside every signed job nonce, rule 3); that manifest lists the installer's source record; the record handed over hashes to that
//      line and names exactly the archive in question (legacy-contract-receipt.mjs archiveOfSeal).
//   2. The host is one this repository trusts: ci/rk-pipeline.json attestation.<platform> gives its key, host name and broker build. The receipt's
//      broker is that build (loop matrix C9: a host whose installed broker is not the committed one cannot be admitted until that list is updated).
//   3. The run's broker jobs: the list is not empty; every listed job's claim is in the bundle with the listed hash, carries the broker's
//      signature for that key, host and broker build, is for that job id, and its signed nonce is the one computed from the run id, the sealed gate,
//      the step and the call (gate-seam.mjs contractJobNonce).
//   4. The launches this checkout's contract-nn.json seals in `signedLaunches` (the launches that cannot be cache hits): each must show a
//      verified job of one of its commands; every verified job of the launch must have the expected signed result and name one same signed
//      commit, which is not the fixture commit when the launch runs on a commit the runner made. A job filed under a launch's label that is
//      another kind of work, another result or another commit does not count and is a problem.
// What this proves, and no more (rustright attested-runs.md section 5): real jobs of this run, for this gate, went through that host's broker.
// The key is readable by the account the broker runs as, so this is not a defence against a hostile program on that host. The receipt's verdict
// itself is the runner's own judgement (`nnBasis`), and the result says so.
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { createHash } from "node:crypto";
import { CONTRACT_FILE, archiveIdentity, archiveOfSeal, verifyReleaseReceipts } from "./legacy-contract-receipt.mjs";
import { contractJobNonce } from "./gate-seam.mjs";
import { verifyPublicationClaim } from "./receipt-signature.mjs";
import { ACCEPTANCE_RESULT_SCHEMA, admitAcceptanceResult, judgeFileHashes } from "./acceptance-admission.mjs";
import { execFileSync } from "node:child_process";

const sha256 = (bytes) => createHash("sha256").update(bytes).digest("hex");
/** The tree id of a commit in this checkout. */
const defaultTreeOf = (commit) => String(execFileSync("git", ["-C", path.dirname(CONTRACT_FILE), "rev-parse", `${commit}^{tree}`], { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"], windowsHide: true })).trim();
const PLATFORM_OF = Object.freeze({ mac: "darwin", dell: "win32" });
const stepIdOf = (label) => String(label).split(":")[0];

/**
 * `receipt`: the contract-test receipt (parsed). `readJob(requestId)`: the bytes of that job's claim from the bundle (throws when absent).
 * `archiveBytes`: the package archive in question. `commit`: the release commit. `pipeline`: ci/rk-pipeline.json (parsed). `sealed`: this
 * checkout's contract-nn.json (parsed). `contractSha256`, `budgetsSha256`, `runnerSha256`, `runnerFiles`: of this checkout. `waivers`: valid waivers.
 * Returns { ok, problems, facts }.
 */
export function admitGateRelease({ receipt, readJob, archiveBytes, commit, pipeline, sealed, contractSha256, budgetsSha256, runnerSha256, runnerFiles, waivers = [], manifestBytes = null, sourceBytes = null }) {
  const problems = [];
  const facts = { host: receipt?.host ?? null, version: receipt?.gate?.version ?? null, sealedSha256: receipt?.gate?.sealedSha256 ?? null, archiveSha256: null, jobsListed: 0, jobsVerified: 0, signedSteps: [], nnBasis: receipt?.nnBasis ?? null };
  const platform = PLATFORM_OF[receipt?.host];
  const pins = platform ? pipeline?.attestation?.[platform] : null;
  if (!platform) problems.push(`the receipt is from ${JSON.stringify(receipt?.host)}, not a host this repository names`);
  else if (!pins || typeof pins.publicKey !== "string" || typeof pins.keyId !== "string" || typeof pins.host !== "string" || typeof pins.brokerDigest !== "string") problems.push(`ci/rk-pipeline.json has no complete attestation.${platform} (publicKey, keyId, host, brokerDigest)`);
  let archive = null;
  try { archive = archiveIdentity(archiveBytes); facts.archiveSha256 = archive.sha256; } catch (e) { problems.push(`the archive in question cannot be hashed (${e.message})`); }
  if (receipt?.gate?.kind !== "candidate") problems.push(`the receipt is a ${JSON.stringify(receipt?.gate?.kind)} gate's, not a candidate's: only a candidate's receipt names the archive it was installed from`);
  // 1 and the broker half of 2: the release check, with everything this checkout and the committed list know.
  problems.push(...verifyReleaseReceipts(receipt, null, { commit, contractSha256, runnerSha256, budgetsSha256, runnerFiles, waivers, sealedSha256: receipt?.gate?.sealedSha256 ?? null, brokerRuntimeDigest: pins?.brokerDigest ?? null, archive: archive ?? { sha256: null, integrity: null } }).problems);
  // 1b: the seal accounts for the archive.
  if (!manifestBytes || !sourceBytes) problems.push("the tested gate's manifest and source record were not handed over, so nothing ties the archive to the seal but the receipt's own statement");
  else {
    const linked = archiveOfSeal({ sealedSha256: receipt?.gate?.sealedSha256, manifestBytes, sourceBytes });
    for (const p of linked.problems) problems.push(`seal to archive: ${p}`);
    if (linked.ok && archive && (linked.archive.sha256 !== archive.sha256 || linked.archive.integrity !== archive.integrity)) problems.push(`seal to archive: the tested gate was installed from the archive ${linked.archive.sha256}, the archive in question is ${archive.sha256}`);
    if (linked.ok && receipt?.gate?.version !== undefined && linked.archive.version !== receipt.gate.version) problems.push(`seal to archive: the source record is for version ${linked.archive.version}, the receipt says ${receipt.gate.version}`);
  }
  if (!commit) problems.push("no release commit was given");
  // 3: the run's broker jobs.
  const jobs = Array.isArray(receipt?.jobs) ? receipt.jobs : null;
  if (typeof receipt?.runId !== "string" || !/^[0-9a-f]{64}$/.test(receipt.runId)) problems.push("the receipt carries no run id, so its jobs cannot be tied to it");
  if (!jobs) problems.push("the receipt lists no broker jobs (jobs is missing)");
  else if (jobs.length === 0) problems.push("the receipt lists no broker job at all: nothing in it is signed");
  const verified = []; // { label, payload } of every job whose claim verified for this run, gate, step and call
  for (const [i, job] of (jobs ?? []).entries()) {
    facts.jobsListed += 1;
    const name = `job ${job?.requestId ?? `#${i + 1}`} (${job?.step})`;
    if (typeof job?.requestId !== "string" || !job.requestId) continue; // refused before it existed: proves nothing, counts for nothing
    if (!/^[A-Za-z0-9._-]{1,128}$/.test(job.requestId)) { problems.push(`${name}: its job id cannot name a file`); continue; }
    if (typeof job.claimSha256 !== "string") { problems.push(`${name}: it carries no publication claim (its broker made none), so nothing about it may be checked here`); continue; }
    let bytes; try { bytes = readJob(job.requestId); } catch (e) { problems.push(`${name}: its claim is not in the bundle (${e.code ?? e.message})`); continue; }
    if (sha256(bytes) !== job.claimSha256) { problems.push(`${name}: the claim in the bundle is not the one listed (sha256 differs)`); continue; }
    let doc; try { doc = JSON.parse(String(bytes)); } catch { problems.push(`${name}: its claim is not JSON`); continue; }
    if (Object.keys(doc ?? {}).some((k) => k !== "id" && k !== "publication")) { problems.push(`${name}: the bundle holds more than the claim (only id and publication may be handed over)`); continue; }
    let payload;
    try { payload = verifyPublicationClaim(doc, pins, job.requestId); } catch (e) { problems.push(`${name}: ${e.code ? `${e.code}: ` : ""}${e.message}`); continue; }
    let want = null; try { want = contractJobNonce({ runId: receipt.runId, sealedSha256: receipt.gate?.sealedSha256, step: job.step, call: job.call }); } catch { /* reported next */ }
    if (want === null || payload.nonce !== want) { problems.push(`${name}: the nonce the broker signed is not this run's for this gate, step and call`); continue; }
    facts.jobsVerified += 1; verified.push({ label: job.step, name, payload });
  }
  facts.signedSteps = [...new Set(verified.map((v) => stepIdOf(v.label)))].sort();
  // 4: the sealed launches. What each must show comes from this checkout's sealed list, never from the receipt.
  const launches = Array.isArray(sealed?.signedLaunches) ? sealed.signedLaunches : null;
  facts.launchesProven = [];
  if (!launches || launches.length === 0) problems.push("this checkout's contract-nn.json seals no launches that must show signed jobs (signedLaunches)");
  else for (const launch of launches) {
    const mine = verified.filter((v) => v.label === launch.label);
    const where = `launch ${launch.label}`;
    if (!mine.length) { problems.push(`${where} cannot be served from a cache and shows no verified broker job`); continue; }
    const bad = [];
    for (const v of mine) {
      const exit = v.payload.result?.exitCode;
      if (exit !== launch.exitCode) bad.push(`${v.name} ended with exit ${exit ?? "(not stated)"}, the launch requires ${launch.exitCode}`);
      // The broker's own word for how the job ended (rustright job.rs JobState::wire). Exit 0 with another word exists: a check after the process
      // did not let the result stand. Cancelled and timed-out jobs never satisfy a launch.
      const word = launch.exitCode === 0 ? "SUCCEEDED" : "FAILED";
      if (v.payload.result?.status !== word) bad.push(`${v.name} ended as ${v.payload.result?.status ?? "(not stated)"}, expected ${word}`);
      if (typeof v.payload.gate?.commit !== "string" || !/^[0-9a-f]{40}$/.test(v.payload.gate.commit)) bad.push(`${v.name} names no commit`);
    }
    const commits = [...new Set(mine.map((v) => v.payload.gate?.commit ?? null))];
    if (commits.length !== 1) bad.push(`its jobs name ${commits.length} different commits`);
    else if (launch.commit === "any" && commits[0] === receipt?.fixtureCommit) bad.push("its jobs ran on the fixture commit, not on a commit made for this launch");
    else if (launch.commit === "fixture" && commits[0] !== receipt?.fixtureCommit) bad.push("its jobs did not run on the fixture commit the receipt states");
    if (!mine.some((v) => Array.isArray(launch.commands) && launch.commands.includes(v.payload.command))) bad.push(`none of its jobs is one of ${(launch.commands ?? []).join(", ")} (found: ${[...new Set(mine.map((v) => String(v.payload.command)))].join(", ")})`);
    if (bad.length) problems.push(...bad.map((b) => `${where}: ${b}`)); else facts.launchesProven.push(launch.label);
  }
  return { ok: problems.length === 0, problems, facts };
}

/**
 * Admission from files on disk: the receipt, the archive in question, the release commit and the committed pipeline file. The folders
 * `job-claims` and `gate-seal` must sit beside the receipt. Returns { ok, problems, facts }; never throws for a missing or bad input.
 */
export function admitFromFiles({ receiptFile, archiveFile, commit, pipelineFile = null, treeOf = defaultTreeOf }) {
  const here = path.dirname(fileURLToPath(import.meta.url));
  const problems = [];
  if (!receiptFile) problems.push("no contract-test receipt was named");
  if (!archiveFile) problems.push("no package archive was named");
  if (!commit || !/^[0-9a-f]{40}$/.test(commit)) problems.push("no 40-hex release commit was given");
  const read = (label, f, parse) => { try { const b = fs.readFileSync(f); return parse ? JSON.parse(String(b)) : b; } catch (e) { problems.push(`${label} ${f} cannot be read (${e.code ?? e.message})`); return null; } };
  const receipt = receiptFile ? read("the receipt", receiptFile, true) : null;
  const archiveBytes = archiveFile ? read("the archive", archiveFile, false) : null;
  const pipeline = read("the pipeline file", pipelineFile ?? path.resolve(here, "..", "..", "ci", "rk-pipeline.json"), true);
  const sealed = read("contract-nn.json", path.join(here, "contract-nn.json"), true);
  if (problems.length) return { ok: false, problems, facts: null };
  // A twelve-line result (GC-A27) is admitted by its own rules; any other record is the old contract-test receipt, read as before so results sealed earlier stay readable.
  if (receipt.schema === ACCEPTANCE_RESULT_SCHEMA) {
    if (receipt.jobClaims !== "job-claims") return { ok: false, problems: [`the result names its job claims folder as ${JSON.stringify(receipt.jobClaims)}, expected "job-claims"`], facts: null };
    const dir = path.dirname(path.resolve(receiptFile));
    const orNull = (f) => { try { return fs.readFileSync(f); } catch { return null; } };
    const seal = receipt.gateSeal === "gate-seal" ? path.join(dir, "gate-seal") : null;
    let tree = null; try { tree = treeOf(commit); } catch (e) { problems.push(`the tree of ${commit} cannot be read (${e.message})`); }
    const contractText = String(fs.readFileSync(CONTRACT_FILE));
    const admitted = admitAcceptanceResult({ result: receipt, readJob: (id) => fs.readFileSync(path.join(dir, "job-claims", `${id}.json`)), archiveBytes, commit, tree, pipeline, contractText, contractSha256: sha256(contractText), judgeFiles: judgeFileHashes(),
      manifestBytes: seal ? orNull(path.join(seal, "manifest.txt")) : null, sourceBytes: seal ? orNull(path.join(seal, "trusted-gate-source.json")) : null });
    return { ...admitted, ok: admitted.ok && problems.length === 0, problems: [...problems, ...admitted.problems] };
  }
  if (receipt.jobClaims !== "job-claims") return { ok: false, problems: [`the receipt names its job claims folder as ${JSON.stringify(receipt.jobClaims)}, expected "job-claims"`], facts: null };
  const beside = path.dirname(path.resolve(receiptFile));
  const readOrNull = (f) => { try { return fs.readFileSync(f); } catch { return null; } };
  const seal = receipt.gateSeal === "gate-seal" ? path.join(beside, "gate-seal") : null;
  // The old test's runner, budgets and contract text are gone from the tree; what an old receipt is judged against is what contract-nn.json recorded of them (`retiredRunner`).
  const retired = sealed?.retiredRunner;
  if (!retired || typeof retired.runnerSha256 !== "string" || typeof retired.budgetsSha256 !== "string" || typeof retired.contractSha256 !== "string" || !Array.isArray(retired.runnerFiles)) return { ok: false, problems: ["this checkout's contract-nn.json records no retired runner (retiredRunner), so a contract-test receipt cannot be read"], facts: null };
  const result = admitGateRelease({ manifestBytes: seal ? readOrNull(path.join(seal, "manifest.txt")) : null, sourceBytes: seal ? readOrNull(path.join(seal, "trusted-gate-source.json")) : null, receipt,
    readJob: (id) => fs.readFileSync(path.join(beside, "job-claims", `${id}.json`)), archiveBytes, commit, pipeline, sealed: retired, contractSha256: retired.contractSha256, budgetsSha256: retired.budgetsSha256, runnerSha256: retired.runnerSha256, runnerFiles: retired.runnerFiles,
    waivers: [] });
  return result;
}

/** The package this rule is about, and the variable a publish entry is handed the receipt through. */
export const GATE_PACKAGE = "@rightkit/release";
export const GATE_RECEIPT_ENV = "RK_GATE_CONTRACT_RECEIPT";
/**
 * NN31 at a publication entry point. Every place that is about to publish an npm archive calls this with the package name, the exact file it
 * is about to publish and the commit it was packed from, BEFORE it starts npm. For any other package it does nothing. For the gate package it
 * throws unless a contract-test receipt was handed over (RK_GATE_CONTRACT_RECEIPT names it; `job-claims` and `gate-seal` sit beside it) and
 * admits exactly that file. There is no flag that skips it.
 */
export function gateReleaseGuard({ name, archiveFile, commit, env = process.env, admit = admitFromFiles, pipelineFile = null }) {
  if (name !== GATE_PACKAGE) return { required: false };
  const receiptFile = env?.[GATE_RECEIPT_ENV];
  if (typeof receiptFile !== "string" || !receiptFile) throw new Error(`${GATE_PACKAGE} is not published without a passing contract-test receipt for this exact archive (GATE-CONTRACT NN31): ${GATE_RECEIPT_ENV} names no receipt`);
  const result = admit({ receiptFile, archiveFile, commit, pipelineFile });
  if (!result.ok) throw new Error(`${GATE_PACKAGE} is not published: its contract-test receipt does not admit this archive (${result.problems.length} problem(s)): ${result.problems.slice(0, 6).join("; ")}${result.problems.length > 6 ? "; ..." : ""}`);
  return { required: true, facts: result.facts };
}

/** `node gate-release-admission.mjs --receipt <file> --archive <package archive> --commit <sha> [--pipeline <ci/rk-pipeline.json>]`: exit 0 only when admitted; with `--record <file>` it then writes the admission record a credentialed upload step requires. The folders `job-claims` and `gate-seal` must sit beside the receipt. */
export function main(argv = process.argv.slice(2), { out = console.log } = {}) {
  const opt = (name) => { const i = argv.indexOf(name); return i < 0 ? undefined : argv[i + 1]; };
  const known = new Set(["--receipt", "--archive", "--commit", "--pipeline", "--record"]);
  for (const a of argv) if (a.startsWith("--") && !known.has(a)) { out(`gate-release-admission REFUSED: unknown option ${a}`); return 64; }
  const problems = [];
  if (!opt("--receipt")) problems.push("--receipt <contract-test receipt> is required");
  if (!opt("--archive")) problems.push("--archive <package archive in question> is required");
  if (!opt("--commit") || !/^[0-9a-f]{40}$/.test(opt("--commit"))) problems.push("--commit <40-hex release commit> is required");
  let result = null;
  if (!problems.length || opt("--receipt") || opt("--archive")) { result = admitFromFiles({ receiptFile: opt("--receipt"), archiveFile: opt("--archive"), commit: opt("--commit"), pipelineFile: opt("--pipeline") ?? null }); if (!problems.length) problems.push(...result.problems); else problems.push(...result.problems.filter((p) => /cannot be read/.test(p))); }
  if (problems.length) { out(`gate-release-admission REFUSED (${problems.length} problem(s))`); for (const p of problems) out(`  ${p}`); return 1; }
  const f = result.facts;
  // --record <file>: the record a credentialed upload step reads (scripts/ci/upload.mjs gateAdmissionProblem). Written only here, only when admitted.
  if (opt("--record")) fs.writeFileSync(opt("--record"), `${JSON.stringify({ schema: "rightkit.gate-release-admission/1", ok: true, unit: GATE_PACKAGE, version: f.version, archiveSha256: f.archiveSha256, commit: opt("--commit"), host: f.host, sealedSha256: f.sealedSha256, jobsVerified: f.jobsVerified, signedSteps: f.signedSteps, nnBasis: f.nnBasis }, null, 2)}\n`);
  out(`gate-release-admission OK: host ${f.host}, sealed gate ${f.sealedSha256}, archive ${f.archiveSha256}; ${f.jobsVerified} of ${f.jobsListed} listed broker job(s) verified, in step(s) ${f.signedSteps.join(", ")}; sealed launch(es) proven from signed claims: ${f.launchesProven.join(", ")}`);
  out(`  every other assertion of the verdict rests on: ${f.nnBasis ?? "not stated"} (the signed claims prove those launches ran on that host's broker for this gate, with that command, result and commit; they do not re-derive the rest)`);
  return 0;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) process.exit(main());
