// The legacy broker carrier of a snapshot door stage's children (rightkit#1301; GATE-CONTRACT "Commit-only build", item 7 rows 7.20 and 7.24-R), as a thin layer over
// stage-seam.mjs (kept by the reset plan). Everything a stage child does goes through stage-seam.mjs; this module only supplies the carrier route the installed broker
// still speaks until the cutover (plan section 7 step 4): `carryBrokerCargo` / `carryBrokerAdapter` (snapshot-broker.mjs) for a call with no seam variables, and the
// gate's local re-verification of the carrier's signed receipts (`verifyBrokerLedger`). Step 5 deletes this module: its importers then import stage-seam.mjs directly,
// and a call with no seam variables is refused there (RK_STAGE_NO_SEAM). The names below are the ones those importers use today.
import { carryBrokerAdapter, carryBrokerCargo, verifyBrokerLedger } from "./snapshot-broker.mjs";
import { installStageCarrier, runStageAdapter, runStageCargo } from "./stage-seam.mjs";

export { CARRIED_CARGO_MAP_ENV, CARRIER_MISSING_CODE, carriedAdapterOutputDir, carriedCargoMapOf, carriedLedgerIds, carriedOutputDir, carriedOutputDirsSince } from "./stage-seam.mjs";

/** The legacy carried cargo request, shaped as a spawnSync result (`outputDir`, `requestId`, `requestDigest` when it verified; `refusal` when refused). */
function carryCargo(request) {
  const result = carryBrokerCargo(request);
  return { status: result.status, signal: null, stdout: result.stdout, stderr: result.stderr, error: null, ...(result.outcome?.status === "verified" ? { outputDir: result.outcome.outputDir, requestId: result.outcome.requestId, requestDigest: result.outcome.requestDigest } : {}), ...(result.outcome?.status === "refused" ? { refusal: { code: result.outcome.code, message: result.outcome.message } } : {}) };
}

/** The legacy carried adapter job, shaped as a spawnSync result (`outputDir`, `requestId`, `nestedChildren`, `executable` when it verified). */
function carryAdapter(request) {
  const result = carryBrokerAdapter(request);
  const verified = result.outcome?.status === "verified";
  return {
    status: result.status, signal: null, stdout: result.stdout, stderr: result.stderr, error: null,
    ...(verified ? { outputDir: result.outcome.outputDir, requestId: result.outcome.requestId, nestedChildren: result.outcome.nestedChildren, ...(result.executable ? { executable: result.executable } : {}) } : {}),
    ...(result.outcome?.status === "refused" ? { refusal: { code: result.outcome.code, message: result.outcome.message } } : {}),
  };
}

/** One right-release Cargo call of a stage child (stage-seam.mjs runStageCargo), with the legacy carrier for a call with no seam variables. */
export function runCarriedCargo(args, options = {}, context = {}) { return runStageCargo(args, options, { ...context, carry: carryCargo }); }

/** One right-release pnpm, node or QA step of a stage child (stage-seam.mjs runStageAdapter), with the legacy carrier for a step with no seam variables. */
export function runCarriedAdapter(adapter, args, options = {}, context = {}) { return runStageAdapter(adapter, args, options, { ...context, carry: carryAdapter }); }

/** The stage map and ledger (stage-seam.mjs installStageCarrier), with the carrier's local re-verification of its signed receipts in `problems(sb)`. */
export function installCarrier(options) { return installStageCarrier({ ...options, verifyLedger: verifyBrokerLedger }); }
