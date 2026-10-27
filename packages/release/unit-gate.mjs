// RightKit's own units use the same door, but never reuse mutable unit receipts as evidence.
import { createHash, randomBytes } from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { localBuildGranted } from "./execution-policy.mjs";
import { readCargoPackage } from "./crate-graph.mjs";
import { spawnSync as nodeSpawnSync } from "./background-process.mjs";
import { endRun, startRun, stagesTo } from "./gate-stages.mjs";
import { qualificationNonce } from "./provenance-verify.mjs";
import { assignJobs, describeCommand, expectedStageCommands } from "./stage-commands.mjs";

const GH = process.platform === "win32" ? "C:\\Program Files\\GitHub CLI\\gh.exe" : "/opt/homebrew/bin/gh";
const defaultRun = (cmd, args, options = {}) => nodeSpawnSync(cmd, args, { encoding: "utf8", maxBuffer: 64 * 1024 * 1024, ...options });
const defaultReadJson = (file) => JSON.parse(fs.readFileSync(file, "utf8"));

/**
 * The gate-owned release of one RightKit unit. Runs only on a broker-managed host and only after this process's check, compile,
 * package and qualify stages: (1) the unit's local pre-flight through the broker (rk-pipeline preflight: the compile rows the dispatch
 * adapter admits), (2) the broker-admitted dispatch of publish.yml (rk-pipeline dispatch -> rustright run rightkit-dispatch; the
 * broker's request id is the run's correlation token), (3) rk-pipeline watch --wait: the exact run (sha, attempt, event, workflow,
 * token), then its publish-direct upload run, then the registry, (4) the upload run's publication receipt (the sealed bytes the
 * registry serves: published && registryVerified) read back against the registry itself. A queued dispatch, a missing upload run,
 * an ambiguous receipt or differing registry bytes is a failure; nothing is retried or re-dispatched here (reconcile rk-pipeline's
 * state by hand). The local bundle's digest is reported beside the hosted one, never equated with it.
 */
export const QUALIFICATION_SCHEMA = "rightkit.unit-qualification/1";
/** The stages whose broker jobs the hosted admission authenticates (their signed receipts ride in the qualification), and how many
 * receipts one stage may carry (every command of the stage needs its own; the dispatch input is capped at 65535 characters in all and
 * each signed envelope is ~3 KB, which writeQualification checks). */
export const ATTESTED_STAGES = ["compile", "package"];
export const MAX_STAGE_RECEIPTS = 64;
const MAX_QUALIFICATION_BYTES = 58_000;

/** `rustright status <id>`: the broker's own record of one job, reduced to what a verifier needs (id + signed envelope). A job the
 * broker cannot report, or one without a signed envelope, fails the stage: an unauthenticated qualification is worth nothing. */
/** The commands each stage must have run for this unit, from the same builders the stages use, over the checkout's own config. */
export function defaultStageCommands({ unit, root, platform, readJson = defaultReadJson }) {
  const manifest = unit.kind === "npm" ? readJson(path.join(root, "packages", unit.name.slice("@rightkit/".length), "package.json")) : undefined;
  return expectedStageCommands({ kind: unit.kind, unit: unit.name, config: readJson(path.join(root, "ci", "gate.config.json")), manifest, platform });
}

export function readBrokerReceipt(id, { run = defaultRun } = {}) {
  const r = run("rustright", ["status", id], { encoding: "utf8", timeout: 60_000 });
  if (r.error || r.status !== 0) throw new Error(`the broker cannot report job ${id} (rustright status failed)`);
  let doc;
  try { doc = JSON.parse(r.stdout); } catch { throw new Error(`the broker's status of job ${id} is not JSON`); }
  const receipt = doc?.receipt ?? doc;
  if (receipt?.id !== id || !receipt.provenance?.payload || !receipt.provenance.signature) throw new Error(`job ${id} has no signed provenance receipt`);
  return { id, provenance: receipt.provenance };
}
/** `<bundle>/qualification.json`: the local stages that passed and the one artifact sha256 they bound, for the dispatcher. */
export function writeQualification({ report, artifact, unit, commit, tree, runtime, executor, write = fs.writeFileSync }) {
  for (const stage of ["check", "compile", "package", "qualify"]) if (report.stages?.[stage]?.ok !== true) throw new Error(`release needs stage ${stage} ok before it runs`);
  if (!artifact?.artifactFile || !/^[0-9a-f]{64}$/.test(artifact.artifactSha256 ?? "")) throw new Error("release needs the qualified artifact");
  const receipt = { schema: QUALIFICATION_SCHEMA, unit: { kind: unit.kind, name: unit.name, version: unit.version }, commit, tree, runtime, executor: executor?.kind ?? null,
    stages: Object.fromEntries(["check", "compile", "package", "qualify"].map((s) => [s, { ...report.stages[s] }])), artifactSha256: artifact.artifactSha256, writtenAt: new Date().toISOString() };
  // The broker's signed receipts for the compile and package stages are what the hosted admission verifies; without them the
  // booleans above are only this process's say-so.
  for (const stage of ATTESTED_STAGES) {
    const list = report.stages[stage].receipts;
    if (!Array.isArray(list) || list.length < 1 || list.length > MAX_STAGE_RECEIPTS) throw new Error(`release needs 1 to ${MAX_STAGE_RECEIPTS} signed broker receipts of the ${stage} stage (found ${Array.isArray(list) ? list.length : "none"})`);
  }
  if (JSON.stringify(receipt).length > MAX_QUALIFICATION_BYTES) throw new Error("the qualification receipt with its broker receipts exceeds the workflow dispatch input limit");
  const file = path.join(path.dirname(artifact.artifactFile), "qualification.json");
  write(file, `${JSON.stringify(receipt, null, 2)}\n`);
  return file;
}

const requireBrokerHost = (executor) => { if (executor?.kind !== "broker") throw new Error("unit release runs only on a broker-managed host (the hosted qualification is dispatched by the RustRight rightkit-dispatch adapter); a direct host has no gate-owned release route, so no upload was started"); };

export async function releaseUnit({ unit, root, commit, executor, env, artifact, qualification, platform = process.platform, run = defaultRun, readJson = defaultReadJson, out = console.log, scratch = platform === "win32" ? "D:\\scratch" : "/Volumes/D/scratch", registryBytes = defaultRegistryBytes }) {
  requireBrokerHost(executor);
  if (typeof qualification !== "string" || !fs.existsSync(qualification)) throw new Error("release needs the unit qualification receipt (written by the gate after the qualify stage); nothing is dispatched without it");
  const registry = unit.kind === "npm" ? "npm" : "crates";
  const pipeline = path.join(root, "scripts", "ci", "rk-pipeline.mjs");
  const stateFile = platform === "win32" ? "D:\\scratch\\rk-pipeline\\state.json" : "/Volumes/D/scratch/rk-pipeline/state.json";
  const step = (label, args, { accept = [0], acceptWhen = null } = {}) => {
    out(`rightkit-gate: [release] ${label}`);
    const r = run(process.execPath, [pipeline, ...args], { cwd: root, env, stdio: ["ignore", "pipe", "pipe"] });
    const accepted = r.status === 0 || (accept.includes(r.status) && (!acceptWhen || acceptWhen(r)));
    if (!accepted) {
      const said = `${String(r.stdout ?? "")}\n${String(r.stderr ?? "")}`.split("\n").map((l) => l.trim()).filter((l) => l && !l.includes("note:")).slice(-4).join(" | ");
      throw new Error(`${label} failed (exit ${r.status}): ${said.slice(0, 600)}`);
    }
    return r;
  };
  const findDispatch = () => {
    const found = Object.values(readJson(stateFile).dispatches ?? {}).filter((d) => d.unit === unit.name && d.sha === commit && d.registry === registry);
    if (found.length > 1) throw new Error(`${found.length} dispatch records for ${unit.name} at ${commit.slice(0, 12)} in rk-pipeline state; reconcile before any retry`);
    return found[0];
  };
  // The pipeline canary (rk-release-canary: the candidate's `right-release doctor` against every app on both hosts, versus the
  // registry's version) runs before the pre-flight. rk-pipeline exits 2 both for "no pipeline canary for this unit" and for any
  // internal error, so 2 is accepted only when it says the former (reviewer, 2026-10-27); a canary error never reads as no canary.
  step(`pipeline canary of ${unit.name} (doctor on every app, both hosts, candidate vs registry)`, ["canary", unit.name],
    { accept: [2], acceptWhen: (r) => /no pipeline canary for this unit/.test(`${r.stdout ?? ""}\n${r.stderr ?? ""}`) });
  step(`local pre-flight of ${unit.name} through the broker`, ["preflight", unit.name, "--registry", registry]);
  step(`broker-admitted hosted qualification dispatch of ${unit.name} at ${commit.slice(0, 12)}`, ["dispatch", unit.name, "--registry", registry, "--qualification", qualification]);
  const dispatched = findDispatch();
  if (!dispatched?.runId || !dispatched.runAttempt) throw new Error(`no dispatch record with a run for ${unit.name} at ${commit.slice(0, 12)}; nothing is claimed (reconcile rk-pipeline state before any retry)`);
  step(`waiting for publish.yml run ${dispatched.runId}, its publish-direct upload run and the registry`, ["watch", "--wait"]);
  const done = findDispatch();
  if (done?.status !== "published" || done.version !== unit.version || !done.uploadRunId) throw new Error(`${unit.name}: the watch ended without a published record (${done?.status ?? "none"}${done?.conclusion ? `: ${done.conclusion}` : ""}); nothing is claimed`);
  const receipt = readPublicationReceipt({ uploadRunId: done.uploadRunId, runId: done.runId, runAttempt: done.runAttempt, run, env, scratch });
  // The receipt is written by the publish-direct UPLOAD run (its runId/runAttempt are that run's: upload.mjs records GITHUB_RUN_ID);
  // the qualification run it consumed is its `candidate` (reviewed-candidate.mjs prepareQualified: id qualified-<run>-<attempt>,
  // runId, runAttempt, sourceSha). Both are bound to what rk-pipeline dispatched and watched, plus commit, registry and repository.
  const candidate = receipt.candidate ?? {};
  const bound = String(receipt.runId) === String(done.uploadRunId)
    && receipt.commit === commit && receipt.registry === registry && receipt.repository === "bogusyogi/rightkit" && receipt.dryRun !== true
    && candidate.source === "qualified" && String(candidate.runId) === String(done.runId) && String(candidate.runAttempt) === String(done.runAttempt) && candidate.sourceSha === commit;
  if (!bound) {
    throw new Error(`${unit.name}@${unit.version}: the publication receipt of upload run ${done.uploadRunId} is not that run's receipt for qualification run ${done.runId} attempt ${done.runAttempt} of ${commit.slice(0, 12)} (${registry}); nothing is claimed`);
  }
  const rows = (receipt.receipts ?? []).filter((r) => r.unit === unit.name && r.version === unit.version);
  if (rows.length !== 1) throw new Error(`${unit.name}@${unit.version}: the publication receipt holds ${rows.length} row(s) for this unit; exactly one is required`);
  const [row] = rows;
  if (row.published !== true || row.registryVerified !== true || row.outcome !== "published" || !/^[0-9a-f]{64}$/.test(row.sha256 ?? "")) throw new Error(`${unit.name}@${unit.version}: the publication receipt of upload run ${done.uploadRunId} records no verified publication (${row.outcome})`);
  // Read the registry back independently of the upload run: npm's dist.shasum (sha1) and the served tarball's sha256 (the receipt
  // row carries sha256 + sha1, not the sha512 integrity); crates: the sparse index cksum (sha256).
  const served = await registryBytes({ unit, registry, run, env, expected: row, scratch });
  const same = registry === "npm" ? served.shasum === row.sha1 && served.sha256 === row.sha256 : served.cksum === row.sha256;
  if (!same) throw new Error(`${unit.name}@${unit.version}: the registry serves different bytes from the upload receipt (served sha256 ${served.sha256 ?? served.cksum ?? "?"}, receipt ${row.sha256})`);
  return {
    registry, runId: done.runId, runAttempt: done.runAttempt, requestId: done.requestId ?? null, uploadRunId: done.uploadRunId,
    hostedSha256: row.sha256, hostedSha1: row.sha1 ?? null, registryVerified: true,
    localArtifactSha256: artifact?.artifactSha256 ?? null, hostedEqualsLocal: artifact ? artifact.artifactSha256 === row.sha256 : null,
  };
}

/** The one `publication-receipt-*` artifact of the upload run, downloaded and parsed. More or fewer than one is ambiguous. */
export function readPublicationReceipt({ uploadRunId, runId, runAttempt, run, env, scratch }) {
  const listed = run(GH, ["api", `repos/bogusyogi/rightkit/actions/runs/${uploadRunId}/artifacts`], { env, encoding: "utf8" });
  if (listed.status !== 0) throw new Error(`cannot list the artifacts of upload run ${uploadRunId}`);
  // publish-direct names its receipt `publication-receipt-<qualification run id>-<its attempt>-<upload attempt>`.
  const pattern = new RegExp(`^publication-receipt-${runId}-${runAttempt}-\\d+$`);
  const names = (JSON.parse(listed.stdout).artifacts ?? []).map((a) => a.name).filter((n) => pattern.test(n));
  if (names.length !== 1) throw new Error(`upload run ${uploadRunId} has ${names.length} publication receipt artifact(s) for run ${runId} attempt ${runAttempt}; exactly one is required`);
  const dir = path.join(scratch, "rk-unit-receipts", `${uploadRunId}-${Date.now()}`);
  fs.rmSync(dir, { recursive: true, force: true });
  fs.mkdirSync(dir, { recursive: true });
  const fetched = run(GH, ["run", "download", String(uploadRunId), "-R", "bogusyogi/rightkit", "--name", names[0], "--dir", dir], { env, encoding: "utf8" });
  if (fetched.status !== 0) throw new Error(`cannot download ${names[0]} of upload run ${uploadRunId}`);
  const file = path.join(dir, "publication-receipt.json");
  if (!fs.existsSync(file)) throw new Error(`${names[0]} holds no publication-receipt.json`);
  return JSON.parse(fs.readFileSync(file, "utf8"));
}

/** What the registry serves for the unit's version: npm dist.integrity + dist.shasum; crates the sparse index cksum. */
export async function defaultRegistryBytes({ unit, registry, run, env, expected, scratch }) {
  if (registry === "npm") {
    const r = run("npm", ["view", `${unit.name}@${unit.version}`, "dist.shasum", "--json", "--prefer-online"], { env, encoding: "utf8" });
    if (r.status !== 0) throw new Error(`npm view ${unit.name}@${unit.version} failed`);
    const shasum = JSON.parse(r.stdout);
    // The served tarball itself, hashed here: `npm pack` downloads exactly what the registry serves for that version.
    const dir = path.join(scratch, "rk-unit-readback", `${unit.name.replace("/", "__")}-${unit.version}-${Date.now()}`);
    fs.mkdirSync(dir, { recursive: true });
    const packed = run("npm", ["pack", `${unit.name}@${unit.version}`, "--pack-destination", dir, "--ignore-scripts"], { env, encoding: "utf8", cwd: dir });
    if (packed.status !== 0) throw new Error(`npm pack ${unit.name}@${unit.version} failed`);
    const files = fs.readdirSync(dir).filter((f) => f.endsWith(".tgz"));
    if (files.length !== 1) throw new Error(`npm pack of ${unit.name}@${unit.version} produced ${files.length} tarball(s)`);
    return { shasum: typeof shasum === "string" ? shasum : shasum?.["dist.shasum"], sha256: hash(fs.readFileSync(path.join(dir, files[0]))) };
  }
  // The sparse index must serve exactly the receipt's sha256 (verifyCargoRegistry throws PUBLICATION_REGISTRY_MISMATCH otherwise).
  const { verifyCargoRegistry } = await import("./cargo-registry-readback.mjs");
  await verifyCargoRegistry({ name: unit.name, version: unit.version, sha256: expected.sha256 });
  return { cksum: expected.sha256 };
}


const hash = (data) => createHash("sha256").update(data).digest("hex");

export function resolveUnit(selector, root, { granted = localBuildGranted, readJson = (p) => JSON.parse(fs.readFileSync(p, "utf8")), readCargo = readCargoPackage } = {}) {
  const match = /^(npm):(@rightkit\/[a-z0-9-]+)$|^(cargo):(rightkit-[a-z0-9-]+)$/.exec(selector ?? "");
  if (!match || !granted(root)) throw new Error("unit needs npm:@rightkit/<name> or cargo:rightkit-<name> in the locally granted RightKit repository");
  const kind = match[1] ?? match[3], name = match[2] ?? match[4];
  const dir = path.join(root, kind === "npm" ? "packages" : "crates", kind === "npm" ? name.slice("@rightkit/".length) : name);
  const manifestFile = path.join(dir, kind === "npm" ? "package.json" : "Cargo.toml");
  if (!fs.realpathSync(manifestFile).startsWith(`${fs.realpathSync(root)}${path.sep}`)) throw new Error("unit manifest escapes the repository");
  const manifest = kind === "npm" ? readJson(path.join(dir, "package.json")) : readCargo(dir);
  if (manifest.name !== name || manifest.private === true || manifest.manifest?.package?.publish === false || typeof manifest.version !== "string" || !/^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/.test(manifest.version)) throw new Error(`invalid canonical manifest for ${selector}`);
  return { kind, name, version: manifest.version };
}

/** One invocation reruns all owed stages; reports are never read as admission evidence. */
export async function runUnitGate({ selector, to, root, commonDir, commit, tree, executor, env, platform, check, loadOwner = () => import("./local-publish.mjs"), out = console.log, err = console.error, releaser = null, resolve = resolveUnit, writeReport, fetchReceipt = readBrokerReceipt, stageCommands = defaultStageCommands }) {
  const stages = stagesTo(to);
  if (!stages) { err(`rightkit-gate: invalid unit stage ${to}`); return 2; }
  let unit;
  try { unit = resolve(selector, root); } catch (error) { err(`rightkit-gate: BLOCKED: ${error.message}`); return 1; }
  const scratch = platform === "win32" ? "D:\\scratch" : "/Volumes/D/scratch";
  const bundle = path.join(scratch, `rk-unit-${randomBytes(12).toString("hex")}`);
  const argv = ["--local", unit.kind === "npm" ? "--package" : "--crate", unit.name, "--workspace", root, "--qualify-only", "--bundle", bundle];
  const runtimeDir = path.dirname(fileURLToPath(import.meta.url));
  const runtimeFiles = [...fs.readdirSync(runtimeDir).filter((name) => name.endsWith(".mjs")).sort().map((name) => path.join(runtimeDir, name)), path.join(runtimeDir, "bin", "rightkit-gate.mjs")];
  const runtime = hash(runtimeFiles.map((file) => `${path.relative(runtimeDir, file)}\0${hash(fs.readFileSync(file))}`).join("\n"));
  const report = { schema: "rightkit.unit-gate-report/1", unit, commit, tree, runtime, executor: executor.kind, stages: {} };
  const save = () => { if (writeReport) writeReport(report); };
  let state, owner, artifact, currentStage, checkTier = null;
  // A run headed to release has its compile and package broker jobs sent with a nonce bound to this commit, tree, unit and stage
  // (RIGHTKIT_ATTEST_NONCE), so each job's signed receipt can be verified hosted for exactly that and nothing else.
  const attested = stages.includes("release") && executor?.kind === "broker";
  const stageEnv = async (stage, action) => {
    const run = startRun({ commonDir, commit, tree, stage });
    const nonce = attested && ATTESTED_STAGES.includes(stage) ? { RIGHTKIT_ATTEST_NONCE: qualificationNonce({ commit, tree, unit: `${unit.kind}:${unit.name}`, stage }) } : {};
    try { return await action({ ...env, ...nonce, ...run.env }); } finally { endRun(run); }
  };
  const receipts = {};
  const collectReceipts = (stage, ids) => {
    if (!attested || !ATTESTED_STAGES.includes(stage)) return;
    // Every job of the stage is fetched; the stage owes a signed receipt for EVERY command it runs (hosted admission requires the same),
    // so a missing one fails here, before any dispatch, naming it. Jobs that are none of the stage's commands are not carried.
    const fetched = [...new Set(ids ?? [])].map((id) => fetchReceipt(id));
    const { assigned, missing } = assignJobs(stageCommands({ unit, root, platform })[stage], fetched.map((r) => ({ id: r.id, payload: r.provenance.payload })));
    if (missing.length) throw new Error(`the ${stage} stage's ${describeCommand(missing[0])} has no broker job receipt (${missing.length} required command(s) missing)`);
    receipts[stage] = fetched.filter((r) => assigned.has(r.id));
  };
  const verifyArtifact = (result) => {
    if (!result || result.kind !== unit.kind || result.name !== unit.name || result.version !== unit.version || result.commit !== commit || !/^[0-9a-f]{64}$/.test(result.artifactSha256 ?? "") || hash(fs.readFileSync(result.artifactFile)) !== result.artifactSha256) throw new Error("unit artifact identity or digest differs from the completed stage");
  };
  try {
    for (const stage of stages) {
      currentStage = stage;
      // A unit run headed to qualify or release owes the release tier: it runs here, before the compile, and qualify does not run it again.
      if (stage === "check") { checkTier = stages.includes("qualify") ? "release" : "compile"; if (await check(checkTier) !== 0) throw new Error(`class A check (${checkTier} tier) failed`); }
      else if (stage === "compile") {
        owner = await loadOwner();
        state = await stageEnv(stage, (runEnv) => owner.compileLocalUnit(unit.kind, argv, { env: runEnv }));
        collectReceipts(stage, owner.localUnitCompileRequests?.(state));
      } else if (stage === "package") {
        artifact = await stageEnv(stage, (runEnv) => owner.packageLocalUnit(state, { env: runEnv }));
        verifyArtifact(artifact);
        collectReceipts(stage, artifact.brokerRequests);
      } else if (stage === "qualify") {
        if (checkTier !== "release" && await check("release") !== 0) throw new Error("release-tier check failed");
        const qualified = await stageEnv(stage, (runEnv) => owner.qualifyLocalUnit(state, { env: runEnv }));
        verifyArtifact(qualified);
        if (!qualified.qualifiedOnly || !qualified.evidencePath || qualified.artifactSha256 !== artifact.artifactSha256) throw new Error("qualification did not bind the packaged artifact");
        artifact = qualified;
      }
      let released = null;
      if (stage === "release") {
        // The qualification receipt the dispatcher carries to publish.yml: the hosted run is bound to these bytes and stages.
        requireBrokerHost(executor); // a direct host has no broker receipts to carry: refuse for that reason, not for the missing receipts
        const qualification = writeQualification({ report, artifact, unit, commit, tree, runtime, executor });
        released = await stageEnv(stage, (runEnv) => (releaser ?? releaseUnit)({ unit, root, commit, executor, env: runEnv, artifact, qualification, platform, out }));
      }
      report.stages[stage] = { ok: true, ...(stage === "package" || stage === "qualify" ? { artifactFile: artifact.artifactFile, artifactSha256: artifact.artifactSha256 } : {}), ...(receipts[stage] ? { receipts: receipts[stage] } : {}), ...(released ?? {}) };
      save();
      out(`rightkit-gate: [${stage}] ${unit.kind}:${unit.name} passed`);
    }
    return 0;
  } catch (error) {
    if (currentStage) { report.stages[currentStage] = { ok: false }; save(); }
    err(`rightkit-gate: unit ${selector} stopped: ${error.message}`);
    return 1;
  } finally { if (state && owner) await owner.closeLocalUnit(state); }
}
