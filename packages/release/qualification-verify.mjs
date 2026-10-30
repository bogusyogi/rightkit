// The verification of a unit qualification's signed broker receipts, in one place for its two callers: the hosted admission
// (scripts/ci/publish-admission.mjs, inside publish.yml) and the gate's own self-check before it dispatches anything (unit-gate.mjs).
// It lives in this package, not under scripts/, because unit-gate ships in the package and may import nothing outside it.
// Pure over the dispatched commit: `git(...args)` reads that commit's objects; nothing here touches the filesystem or the broker.
import { ProvenanceError, carriedCallNonce, qualificationNonce, verifyProvenance } from "./provenance-verify.mjs";
import { SnapshotRequestError, verifySnapshotEvidenceIdentity } from "./snapshot-request.mjs";
import { assignJobs, describeCommand, expectedStageCommands, rootOfWorkspace } from "./stage-commands.mjs";

export const ATTESTED_STAGES = ["compile", "package"];
export const MAX_VERIFIED_RECEIPTS = 64;
const SHA256_HEX = /^[0-9a-f]{64}$/;
const isRecord = (v) => v !== null && typeof v === "object" && !Array.isArray(v);
const sameJson = (a, b) => JSON.stringify(a) === JSON.stringify(b);

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
 */
export function verifyQualificationReceipts(text, { sha, git }) {
  let q;
  try { q = JSON.parse(text); } catch { refuse("the qualification input is not JSON"); }
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
  const seen = new Set();
  const verified = {};
  for (const stage of ATTESTED_STAGES) {
    const list = q.stages?.[stage]?.receipts;
    requireThat(Array.isArray(list) && list.length >= 1, `the qualification receipt's ${stage} stage carries no signed broker receipts; an unauthenticated qualification admits nothing`);
    requireThat(list.length <= MAX_VERIFIED_RECEIPTS, `the qualification receipt's ${stage} stage carries ${list.length} receipts (at most ${MAX_VERIFIED_RECEIPTS})`);
    const stageNonce = qualificationNonce({ commit: sha, tree, unit, stage });
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
      let payload, failure, pin;
      for (const candidate of candidates) {
        for (const nonce of candidateNonces(candidate, receipt.provenance?.payload?.request)) {
          try { payload = verifyProvenance(receipt, candidate, { jobId: receipt.id, nonce }); pin = candidate; break; } catch (error) { if (!(error instanceof ProvenanceError)) throw error; failure = error; }
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
      // The signed snapshot evidence names the commit, tree and job and shows the snapshot held the tree throughout (the same pure
      // function the gate applies to its own receipts), and one snapshot and one ownership handle carry every job of the qualification.
      const evidence = payload.snapshotEvidence;
      requireThat(isRecord(evidence), `${where} (job ${receipt.id}) carries no signed snapshotEvidence: it was not built as a snapshot request of the dispatched commit`);
      try { verifySnapshotEvidenceIdentity(evidence, { commit: { sha, tree }, tree, jobId: receipt.id }); }
      catch (error) { if (!(error instanceof SnapshotRequestError)) throw error; refuse(`${where} (job ${receipt.id}): ${error.message}`); }
      const path = evidence.snapshot?.path, owner = evidence.ownership?.handle;
      requireThat(typeof path === "string" && path !== "" && typeof owner === "string" && owner !== "", `${where} (job ${receipt.id}) names no snapshot path or ownership handle`);
      requireThat(snapshotPath === null || snapshotPath === path, `${where} (job ${receipt.id}) ran in the snapshot ${path}, another snapshot than the qualification's other jobs (${snapshotPath})`);
      requireThat(handle === null || handle === owner, `${where} (job ${receipt.id}) was signed under ownership handle ${owner}, another than the qualification's other jobs (${handle})`);
      snapshotPath = path; handle = owner;
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
      requireThat(job.payload.nonce === nonceFor(row, requestedArgv), `${stage} job ${job.id} carries a nonce that is not the one its command (${describeCommand(row)}) owes`);
      // The primary root: the signed workspace without the directory of the row's command (a carried cargo call runs from its manifest's
      // directory, a pnpm job at the root). Every job of the qualification must show the same one, and so must its workspaceRoot.
      const root = rootOfWorkspace(workspace, row.cwd);
      requireThat(root !== null, `${stage} job ${job.id} ran in ${workspace}, which is not the directory ${row.cwd} of a checkout`);
      requireThat(workspaceRoot === undefined || rootOfWorkspace(workspaceRoot, row.cwd) === root, `${stage} job ${job.id} ran in ${workspace} with workspace root ${workspaceRoot}, not in one checkout`);
      requireThat(primaryRoot === null || primaryRoot === root, `${stage} job ${job.id} ran in ${workspace}, another workspace than the qualification's other jobs (${primaryRoot})`);
      primaryRoot = root;
    }
    verified[stage] = list.map((receipt) => summaries.get(receipt.id));
  }
  return verified;
}
