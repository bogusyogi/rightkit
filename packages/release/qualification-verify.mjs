// The verification of a unit qualification's signed broker receipts, in one place for its two callers: the hosted admission
// (scripts/ci/publish-admission.mjs, inside publish.yml) and the gate's own self-check before it dispatches anything (unit-gate.mjs).
// It lives in this package, not under scripts/, because unit-gate ships in the package and may import nothing outside it.
// Pure over the dispatched commit: `git(...args)` reads that commit's objects; nothing here touches the filesystem or the broker.
import path from "node:path";
import { ProvenanceError, verifyProvenanceSignature } from "./receipt-signature.mjs";
// LEGACY BRANCH ONLY (carrier-era receipts: the caller nonce and signed snapshotEvidence). Step 5 of the reset plan deletes these two imports with the branch.
import { carriedCallNonce, qualificationNonce, verifyProvenance } from "./provenance-verify.mjs";
import { SnapshotRequestError, ownershipOfHandle, verifySnapshotEvidenceIdentity } from "./snapshot-request.mjs";
import { assignJobs, describeCommand, expectedStageCommands, rootOfWorkspace } from "./stage-commands.mjs";

export const ATTESTED_STAGES = ["compile", "package"];
export const MAX_VERIFIED_RECEIPTS = 64;
const SHA256_HEX = /^[0-9a-f]{64}$/;
const isRecord = (v) => v !== null && typeof v === "object" && !Array.isArray(v);
const sameJson = (a, b) => JSON.stringify(a) === JSON.stringify(b);
const GIT_ID = /^[0-9a-f]{40}$/;
const isAbsolutePath = (p) => typeof p === "string" && p !== "" && (path.posix.isAbsolute(p.replaceAll("\\", "/")) || /^[A-Za-z]:[\\/]/.test(p));
const posixOf = (p) => String(p ?? "").replaceAll("\\", "/").replace(/\/+$/, "");

const refuse = (message) => { throw new Error(`publication admission refused: ${message}`); };
const requireThat = (condition, message) => { if (!condition) refuse(message); };

/** The enrolled host pins committed at `sha` (ci/rk-pipeline.json "attestation", one entry per platform) -- never taken from the request. */
export function enrolledPins(sha, git) {
  let doc;
  try { doc = JSON.parse(git("show", `${sha}:ci/rk-pipeline.json`)); } catch { refuse("ci/rk-pipeline.json is unreadable at the dispatched commit; no host is enrolled"); }
  const pins = Object.entries(doc?.attestation ?? {})
    .filter(([, p]) => p && typeof p === "object" && ["publicKey", "keyId", "host", "brokerDigest"].every((key) => typeof p[key] === "string" && p[key]))
    .map(([platform, { publicKey, keyId, host, brokerDigest }]) => ({ publicKey, keyId, host, brokerDigest, platform }));
  requireThat(pins.length > 0, "ci/rk-pipeline.json enrolls no attestation pins at the dispatched commit");
  return pins;
}

/**
 * The qualification's broker receipts, verified. For each of the compile and package stages: at least one receipt whose signature
 * verifies against the pins enrolled at the dispatched commit for the receipt's host, whose job id is its own, and whose signed result is
 * SUCCEEDED with exit code 0. The nonce binds the dispatched commit, its real git tree, the unit and the stage: a carried call (a cargo call, or a pnpm
 * adapter job) carries the per-call nonce derived from it, the directory it ran in and its argv (carriedCallNonce, rightkit#1301 B8),
 * checked against the stage row the job is assigned to. A receipt of another commit, tree, unit or stage, or of another
 * command or directory, cannot carry the nonce. Every receipt also carries signed snapshotEvidence (B9): the commit and tree equal the
 * dispatched commit's, the snapshot held that tree at start and at exit, and its request id is the job's; all receipts share one snapshot
 * path and one ownership handle, and, with the stage row's directory removed, one primary root. A job id is used once across stages.
 * The qualification's tree must equal the dispatched commit's tree. Returns the verified summary the admission receipt records.
 *
 * TWO RECEIPT SHAPES (reset plan section 3; the legacy one goes at plan step 5):
 *  - SEAM receipt (the payload carries `gate`): signature against the pin enrolled at the dispatched commit for the receipt's host; payload.jobId === receipt.id; SUCCEEDED with
 *    exit 0; signed gate.commit and gate.tree are the dispatched commit's and its real git tree; gate.snapshotUnchanged is true; one gate.snapshot and one directory of Class A
 *    verdicts (every verdict named for this commit) across all receipts; one host; a job id used once; each signed request is one of the stage's own commands (exact argv and
 *    directory, stage-commands.mjs assignJobs) and EVERY command of the stage is covered; the request's checkout (workspace without the row's directory) is gate.snapshot itself.
 *    WHAT THE REMOVED CALLER NONCE BOUND, AND WHAT BINDS IT NOW:
 *      commit and tree          nonce (derived from them)           -> signed gate.commit, gate.tree (broker-written from the admitted request and the snapshot state; stronger: not a value the caller chose)
 *      the command and its dir  carriedCallNonce (cwd + argv)       -> signed request.requestedArgv/workspace matched to the stage row (assignJobs) and the root rule
 *      one run, one snapshot    snapshotEvidence snapshot path      -> signed gate.snapshot equal across receipts and equal to the request's own checkout
 *      the snapshot held the tree  snapshotEvidence start/exit      -> signed gate.snapshotUnchanged (the broker's post-job tree check)
 *      Class A ran first        (not bound by CI before)            -> signed gate.classA names a verdict of this commit (only the file name is checked here; the broker admitted the job on its content)
 *    NOT BOUND ANY MORE (a weakening, reported, not hidden):
 *      - the UNIT and the STAGE label: the nonce hashed `unit` and `stage` in. A seam receipt carries neither; it is tied to the unit and stage only through its argv and directory
 *        matching one command of that stage. A receipt of an identical command (same argv and directory) of another unit or of the other stage would satisfy this one.
 *      - ONE DOOR RUN: the ownership handle (one per door run) is gone. `gate.snapshot` is `<data root>/snapshots/<repo-key>`, the same path for every run of the repository on a host,
 *        so receipts of two separate runs of this commit on one host are indistinguishable. Both are genuine broker runs of the dispatched commit and tree.
 *      - the caller's freshness: the old nonce was derived from public values, so it never proved recency either; nothing here does (terminalAt is signed but not judged).
 *  - CARRIER-ERA receipt (the payload carries snapshotEvidence): the rules above this block, unchanged. A qualification is one shape; a mix is refused.
 */
export function verifyQualificationReceipts(text, { sha, git }) {
  let q;
  try { q = JSON.parse(text); } catch { refuse("the qualification input is not JSON"); }
  return verifyReceiptStages(q, { sha, git, stages: ATTESTED_STAGES, nonceOf: ({ commit, tree, unit, stage }) => qualificationNonce({ commit, tree, unit, stage }) });
}

// CI supplies its independently bound nonce; publication always uses the fixed wrapper above.
export function verifyCiCompileReceipts(q, { sha, git, stageNonce }) {
  requireThat(typeof stageNonce === "string" && SHA256_HEX.test(stageNonce), "the CI compile nonce is invalid");
  return verifyReceiptStages(q, { sha, git, stages: ["compile"], nonceOf: () => stageNonce });
}

function verifyReceiptStages(q, { sha, git, stages, nonceOf }) {
  const tree = String(git("rev-parse", `${sha}^{tree}`)).trim();
  requireThat(/^[0-9a-f]{40}$/.test(tree), `cannot read the tree of ${sha.slice(0, 12)}`);
  requireThat(q?.tree === tree, `the qualification receipt is for tree ${String(q?.tree).slice(0, 12)}, the dispatched commit's tree is ${tree.slice(0, 12)}`);
  requireThat(typeof q.unit?.kind === "string" && typeof q.unit?.name === "string", "the qualification receipt names no unit");
  const pins = enrolledPins(sha, git);
  const unit = `${q.unit.kind}:${q.unit.name}`;
  // What each stage's signed requests must show, derived by the stages' own command builders from the dispatched commit's files.
  const expectedByPlatform = new Map();
  const expectedFor = (platform) => {
    if (!expectedByPlatform.has(platform)) {
      let expected;
      try {
        const config = JSON.parse(git("show", `${sha}:ci/gate.config.json`));
        const manifest = q.unit.kind === "npm" ? JSON.parse(git("show", `${sha}:packages/${q.unit.name.slice("@rightkit/".length)}/package.json`)) : undefined;
        expected = expectedStageCommands({ kind: q.unit.kind, unit: q.unit.name, config, manifest, platform });
      } catch (error) { refuse(`cannot derive the ${platform} stage commands of ${unit} at ${sha.slice(0, 12)}: ${error.message}`); }
      expectedByPlatform.set(platform, expected);
    }
    return expectedByPlatform.get(platform);
  };
  let platform = null;
  let primaryRoot = null, snapshotPath = null, handle = null;
  let shape = null, seamSnapshot = null, seamVerdictDir = null;
  const seen = new Set();
  const verified = {};
  for (const stage of stages) {
    const list = q.stages?.[stage]?.receipts;
    requireThat(Array.isArray(list) && list.length >= 1, `the qualification receipt's ${stage} stage carries no signed broker receipts; an unauthenticated qualification admits nothing`);
    requireThat(list.length <= MAX_VERIFIED_RECEIPTS, `the qualification receipt's ${stage} stage carries ${list.length} receipts (at most ${MAX_VERIFIED_RECEIPTS})`);
    const stageNonce = nonceOf({ commit: sha, tree, unit, stage });
    // The nonces a receipt may carry, before its row is known: every carried call (a Cargo call, and a pnpm adapter job, rightkit#1301 B8) the
    // nonce derived for the directory of each of the stage's rows it could be and the argv it signed. The row it is assigned to (below) must
    // then be the one that fits.
    const nonceFor = (row, argv) => carriedCallNonce({ stageNonce, cwd: row.cwd ?? ".", argv });
    // The stage nonce is always tried as well, so a job that is none of the stage's commands is refused for that (below), not for its nonce.
    const candidateNonces = (host, request) => {
      if ((request?.kind !== "cargo" && request?.kind !== "adapter") || !Array.isArray(request.requestedArgv)) return [stageNonce];
      // A cargo row is told apart by its exact argv; an adapter row's argv can hold a scratch path (pack), so every adapter row is tried (a job
      // of another adapter then reaches the command check below and is refused for that, not for its nonce).
      const rows = expectedFor(host.platform)[stage].filter((row) => row.kind === request.kind && (row.kind === "adapter" || row.argvs.some((argv) => sameJson(argv, request.requestedArgv))));
      return [...new Set([...rows.map((row) => nonceFor(row, request.requestedArgv)), stageNonce])];
    };
    const jobs = [];
    const summaries = new Map();
    list.forEach((receipt, index) => {
      const where = `${stage} receipt ${index + 1}`;
      requireThat(receipt && typeof receipt === "object" && typeof receipt.id === "string" && receipt.id, `${where} has no job id`);
      requireThat(!seen.has(receipt.id), `${where}: job ${receipt.id} appears more than once in the qualification`);
      seen.add(receipt.id);
      const candidates = pins.filter((p) => p.host === receipt.provenance?.payload?.host);
      requireThat(candidates.length > 0, `${where}: signed for host ${JSON.stringify(receipt.provenance?.payload?.host)}, which ci/rk-pipeline.json does not enroll`);
      // The receipt's shape is read from the envelope's payload and then proven by the signature: a seam receipt (reset plan section 3) carries the signed `gate`
      // object; a carrier-era receipt carries `snapshotEvidence`. One qualification is one shape (a door run is one or the other).
      const seam = isRecord(receipt.provenance?.payload) && Object.hasOwn(receipt.provenance.payload, "gate");
      requireThat(shape === null || shape === (seam ? "seam" : "legacy"), `${where} (job ${receipt.id}) is a ${seam ? "seam" : "carrier-era"} receipt in a qualification of ${shape} receipts; one qualification is one door run`);
      shape = seam ? "seam" : "legacy";
      let payload, failure, pin;
      for (const candidate of candidates) {
        if (seam) {
          // No caller nonce exists for a seam receipt (the client mints it internally): the signature, the pinned host and broker, and the job id are checked, nothing else here.
          try { payload = verifyProvenanceSignature(receipt, candidate, receipt.id); pin = candidate; } catch (error) { if (!(error instanceof ProvenanceError)) throw error; failure = error; }
        } else {
          for (const nonce of candidateNonces(candidate, receipt.provenance?.payload?.request)) {
            try { payload = verifyProvenance(receipt, candidate, { jobId: receipt.id, nonce }); pin = candidate; break; } catch (error) { if (!(error instanceof ProvenanceError)) throw error; failure = error; }
          }
        }
        if (payload) break;
      }
      requireThat(payload, `${where} (job ${receipt.id}) does not verify against the enrolled pins at ${sha.slice(0, 12)}: ${failure?.code}: ${failure?.message}`);
      requireThat(payload.result?.status === "SUCCEEDED" && payload.result.exitCode === 0, `${where} (job ${receipt.id}) did not succeed (${payload.result?.status}, exit ${payload.result?.exitCode})`);
      requireThat(typeof payload.request?.workspace === "string" && payload.request.workspace !== "", `${where} (job ${receipt.id}) records no workspace`);
      requireThat(SHA256_HEX.test(payload.requestDigest ?? ""), `${where} (job ${receipt.id}) carries no request digest`);
      // The signed request must be one of this stage's own commands (kind, adapter and exact argv, as the stage's builders define them),
      // run on the enrolled host: the same one for every receipt of the qualification.
      platform ??= pin.platform;
      requireThat(pin.platform === platform, `${where} (job ${receipt.id}) was signed by the ${pin.platform} host, another host than the qualification's other jobs (${platform})`);
      if (seam) {
        // The seam receipt's signed `gate` object (the broker writes it from the admitted request, the snapshot state and its own post-job tree check): this commit, this tree,
        // the snapshot unchanged. See the binding comment above verifyQualificationReceipts for what this does and does not bind.
        const gate = payload.gate;
        requireThat(isRecord(gate), `${where} (job ${receipt.id}) carries no signed gate object`);
        requireThat(gate.commit === sha, `${where} (job ${receipt.id}) was built from commit ${String(gate.commit).slice(0, 12)}, not the dispatched ${sha.slice(0, 12)}`);
        requireThat(gate.tree === tree, `${where} (job ${receipt.id}) was built from tree ${String(gate.tree).slice(0, 12)}, the dispatched commit's tree is ${tree.slice(0, 12)}`);
        requireThat(gate.snapshotUnchanged === true, `${where} (job ${receipt.id}) did not leave the snapshot unchanged (gate.snapshotUnchanged is ${JSON.stringify(gate.snapshotUnchanged)})`);
        requireThat(isAbsolutePath(gate.snapshot) && isAbsolutePath(gate.classA), `${where} (job ${receipt.id}) names no absolute snapshot or Class A verdict path`);
        // The verdict file's name carries its commit (gate-seam.mjs verdictFileName: class-a-<commit12>-<scope>-<workspace>.json); every call of a door run names a verdict of
        // that commit, derived from the one root verdict, so all of them sit in one directory.
        const verdict = posixOf(gate.classA);
        requireThat(path.posix.basename(verdict).startsWith(`class-a-${sha.slice(0, 12)}-`), `${where} (job ${receipt.id}) names the Class A verdict ${path.posix.basename(verdict)}, not one of commit ${sha.slice(0, 12)}`);
        const verdictDir = path.posix.dirname(verdict);
        requireThat(seamSnapshot === null || seamSnapshot === posixOf(gate.snapshot), `${where} (job ${receipt.id}) ran in the snapshot ${gate.snapshot}, another snapshot than the qualification's other jobs (${seamSnapshot})`);
        requireThat(seamVerdictDir === null || seamVerdictDir === verdictDir, `${where} (job ${receipt.id}) names a Class A verdict in ${verdictDir}, another directory than the qualification's other jobs (${seamVerdictDir})`);
        seamSnapshot = posixOf(gate.snapshot); seamVerdictDir = verdictDir;
      } else {
        // LEGACY BRANCH (carrier-era receipts; deleted at step 5 of the reset plan with the carrier, snapshot-request.mjs and provenance-verify.mjs).
        // The signed snapshot evidence names the commit, tree and job and shows the snapshot held the tree throughout (the same pure
        // function the gate applies to its own receipts), and one snapshot and one ownership handle carry every job of the qualification.
        const evidence = payload.snapshotEvidence;
        requireThat(isRecord(evidence), `${where} (job ${receipt.id}) carries no signed snapshotEvidence: it was not built as a snapshot request of the dispatched commit`);
        try { verifySnapshotEvidenceIdentity(evidence, { commit: { sha, tree }, tree, jobId: receipt.id }); }
        catch (error) { if (!(error instanceof SnapshotRequestError)) throw error; refuse(`${where} (job ${receipt.id}): ${error.message}`); }
        // Each carried request signs its own sub-handle `<ownership-handle>#<n>`; the qualification's one ownership is the part before the `#`.
        const path_ = evidence.snapshot?.path, owner = ownershipOfHandle(evidence.ownership?.handle);
        requireThat(typeof path_ === "string" && path_ !== "" && typeof owner === "string" && owner !== "", `${where} (job ${receipt.id}) names no snapshot path or ownership handle`);
        requireThat(snapshotPath === null || snapshotPath === path_, `${where} (job ${receipt.id}) ran in the snapshot ${path_}, another snapshot than the qualification's other jobs (${snapshotPath})`);
        requireThat(handle === null || handle === owner, `${where} (job ${receipt.id}) was signed under ownership handle ${owner}, another than the qualification's other jobs (${handle})`);
        snapshotPath = path_; handle = owner;
      }
      jobs.push({ id: receipt.id, payload });
      summaries.set(receipt.id, { jobId: receipt.id, host: payload.host, keyId: payload.keyId, requestDigest: payload.requestDigest, requestedArgv: payload.request.requestedArgv });
    });
    // EVERY command the stage runs must be covered by a verified receipt of its own job, and every receipt must be one of those commands.
    const { assigned, unmatched, missing } = assignJobs(expectedFor(platform)[stage], jobs);
    // Messages are formatted only on failure: on success there is no unmatched job or missing command to describe.
    if (unmatched.length) requireThat(false, `${stage} job ${unmatched[0]} ran ${JSON.stringify(jobs.find((j) => j.id === unmatched[0])?.payload.request.requestedArgv)}, which is not (or no longer) a command of the ${stage} stage of ${unit}`);
    if (missing.length) requireThat(false, `the qualification carries no signed receipt of the ${stage} stage's ${missing.length ? describeCommand(missing[0]) : ""} (${missing.length} of ${assigned.size + missing.length} required command(s) missing)`);
    for (const job of jobs) {
      const row = assigned.get(job.id);
      const { workspace, workspaceRoot, requestedArgv } = job.payload.request;
      // The nonce is the one this row's command owes: derived from the directory its row runs in and the argv it signed (a Cargo call or a pnpm job).
      // (LEGACY BRANCH: a seam receipt has no caller nonce; its command binding is the argv/cwd assignment above plus the signed gate commit and tree.)
      if (shape === "legacy") requireThat(job.payload.nonce === nonceFor(row, requestedArgv), `${stage} job ${job.id} carries a nonce that is not the one its command (${describeCommand(row)}) owes`);
      // The primary root: the signed workspace without the directory of the row's command (a carried cargo call runs from its manifest's
      // directory, a pnpm job at the root). Every job of the qualification must show the same one, and so must its workspaceRoot.
      const root = rootOfWorkspace(workspace, row.cwd);
      requireThat(root !== null, `${stage} job ${job.id} ran in ${workspace}, which is not the directory ${row.cwd} of a checkout`);
      requireThat(workspaceRoot === undefined || workspaceRoot === null || rootOfWorkspace(workspaceRoot, row.cwd) === root, `${stage} job ${job.id} ran in ${workspace} with workspace root ${workspaceRoot}, not in one checkout`);
      requireThat(primaryRoot === null || primaryRoot === root, `${stage} job ${job.id} ran in ${workspace}, another workspace than the qualification's other jobs (${primaryRoot})`);
      // A seam job's primary root is the snapshot its signed gate object names: the directory the request ran in lies in that very snapshot, so the request
      // and the gate evidence describe one checkout.
      if (shape === "seam") requireThat(root === seamSnapshot, `${stage} job ${job.id} ran in ${workspace}, whose checkout ${root} is not the snapshot ${seamSnapshot} its gate evidence names`);
      primaryRoot = root;
    }
    verified[stage] = list.map((receipt) => summaries.get(receipt.id));
  }
  return verified;
}
