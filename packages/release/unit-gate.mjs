// RightKit's own units use the same door, but never reuse mutable unit receipts as evidence.
import { createHash, randomBytes } from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { localBuildGranted } from "./execution-policy.mjs";
import { readCargoPackage } from "./crate-graph.mjs";
import { spawnSync as nodeSpawnSync } from "node:child_process";
import { endRun, startRun, stagesTo } from "./gate-stages.mjs";

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
export async function releaseUnit({ unit, root, commit, executor, env, artifact, platform = process.platform, run = defaultRun, readJson = defaultReadJson, out = console.log, scratch = platform === "win32" ? "D:\\scratch" : "/Volumes/D/scratch", registryBytes = defaultRegistryBytes }) {
  if (executor?.kind !== "broker") throw new Error("unit release runs only on a broker-managed host (the hosted qualification is dispatched by the RustRight rightkit-dispatch adapter); a direct host has no gate-owned release route, so no upload was started");
  const registry = unit.kind === "npm" ? "npm" : "crates";
  const pipeline = path.join(root, "scripts", "ci", "rk-pipeline.mjs");
  const stateFile = platform === "win32" ? "D:\\scratch\\rk-pipeline\\state.json" : "/Volumes/D/scratch/rk-pipeline/state.json";
  const step = (label, args) => {
    out(`rightkit-gate: [release] ${label}`);
    const r = run(process.execPath, [pipeline, ...args], { cwd: root, env, stdio: ["ignore", "pipe", "pipe"] });
    if (r.status !== 0) throw new Error(`${label} failed (exit ${r.status}): ${String(r.stderr ?? "").trim().split("\n").slice(-3).join(" | ").slice(0, 400)}`);
    return r;
  };
  const findDispatch = () => Object.values(readJson(stateFile).dispatches ?? {}).find((d) => d.unit === unit.name && d.sha === commit && d.registry === registry);
  step(`local pre-flight of ${unit.name} through the broker`, ["preflight", unit.name, "--registry", registry]);
  step(`broker-admitted hosted qualification dispatch of ${unit.name} at ${commit.slice(0, 12)}`, ["dispatch", unit.name, "--registry", registry]);
  const dispatched = findDispatch();
  if (!dispatched?.runId || !dispatched.runAttempt) throw new Error(`no dispatch record with a run for ${unit.name} at ${commit.slice(0, 12)}; nothing is claimed (reconcile rk-pipeline state before any retry)`);
  step(`waiting for publish.yml run ${dispatched.runId}, its publish-direct upload run and the registry`, ["watch", "--wait"]);
  const done = findDispatch();
  if (done?.status !== "published" || done.version !== unit.version || !done.uploadRunId) throw new Error(`${unit.name}: the watch ended without a published record (${done?.status ?? "none"}${done?.conclusion ? `: ${done.conclusion}` : ""}); nothing is claimed`);
  const receipt = readPublicationReceipt({ uploadRunId: done.uploadRunId, run, env, scratch });
  const row = (receipt.receipts ?? []).find((r) => r.unit === unit.name && r.version === unit.version);
  if (!row || row.published !== true || row.registryVerified !== true || row.outcome !== "published") throw new Error(`${unit.name}@${unit.version}: the publication receipt of upload run ${done.uploadRunId} records no verified publication (${row ? row.outcome : "no row"})`);
  const served = await registryBytes({ unit, registry, run, env, expected: row });
  const same = registry === "npm" ? served.integrity === row.integrity && served.shasum === row.sha1 : served.cksum === row.sha256;
  if (!same) throw new Error(`${unit.name}@${unit.version}: the registry serves different bytes from the upload receipt`);
  return {
    registry, runId: done.runId, runAttempt: done.runAttempt, requestId: done.requestId ?? null, uploadRunId: done.uploadRunId,
    hostedSha256: row.sha256, integrity: row.integrity ?? null, registryVerified: true,
    localArtifactSha256: artifact?.artifactSha256 ?? null, hostedEqualsLocal: artifact ? artifact.artifactSha256 === row.sha256 : null,
  };
}

/** The one `publication-receipt-*` artifact of the upload run, downloaded and parsed. More or fewer than one is ambiguous. */
export function readPublicationReceipt({ uploadRunId, run, env, scratch }) {
  const listed = run(GH, ["api", `repos/bogusyogi/rightkit/actions/runs/${uploadRunId}/artifacts`], { env, encoding: "utf8" });
  if (listed.status !== 0) throw new Error(`cannot list the artifacts of upload run ${uploadRunId}`);
  const names = (JSON.parse(listed.stdout).artifacts ?? []).map((a) => a.name).filter((n) => n.startsWith("publication-receipt-"));
  if (names.length !== 1) throw new Error(`upload run ${uploadRunId} has ${names.length} publication receipt artifact(s); exactly one is required`);
  const dir = path.join(scratch, "rk-unit-receipts", String(uploadRunId));
  fs.mkdirSync(dir, { recursive: true });
  const fetched = run(GH, ["run", "download", String(uploadRunId), "-R", "bogusyogi/rightkit", "--name", names[0], "--dir", dir], { env, encoding: "utf8" });
  if (fetched.status !== 0) throw new Error(`cannot download ${names[0]} of upload run ${uploadRunId}`);
  const file = path.join(dir, "publication-receipt.json");
  if (!fs.existsSync(file)) throw new Error(`${names[0]} holds no publication-receipt.json`);
  return JSON.parse(fs.readFileSync(file, "utf8"));
}

/** What the registry serves for the unit's version: npm dist.integrity + dist.shasum; crates the sparse index cksum. */
export async function defaultRegistryBytes({ unit, registry, run, env, expected }) {
  if (registry === "npm") {
    const r = run("npm", ["view", `${unit.name}@${unit.version}`, "dist.integrity", "dist.shasum", "--json", "--prefer-online"], { env, encoding: "utf8" });
    if (r.status !== 0) throw new Error(`npm view ${unit.name}@${unit.version} failed`);
    const dist = JSON.parse(r.stdout);
    return { integrity: dist["dist.integrity"], shasum: dist["dist.shasum"] };
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
export async function runUnitGate({ selector, to, root, commonDir, commit, tree, executor, env, platform, check, loadOwner = () => import("./local-publish.mjs"), out = console.log, err = console.error, releaser = null, resolve = resolveUnit, writeReport }) {
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
  let state, owner, artifact, currentStage;
  const stageEnv = async (stage, action) => {
    const run = startRun({ commonDir, commit, tree, stage });
    try { return await action({ ...env, ...run.env }); } finally { endRun(run); }
  };
  const verifyArtifact = (result) => {
    if (!result || result.kind !== unit.kind || result.name !== unit.name || result.version !== unit.version || result.commit !== commit || !/^[0-9a-f]{64}$/.test(result.artifactSha256 ?? "") || hash(fs.readFileSync(result.artifactFile)) !== result.artifactSha256) throw new Error("unit artifact identity or digest differs from the completed stage");
  };
  try {
    for (const stage of stages) {
      currentStage = stage;
      if (stage === "check") { if (await check("compile") !== 0) throw new Error("class A check failed"); }
      else if (stage === "compile") {
        owner = await loadOwner();
        state = await stageEnv(stage, (runEnv) => owner.compileLocalUnit(unit.kind, argv, { env: runEnv }));
      } else if (stage === "package") {
        artifact = await stageEnv(stage, (runEnv) => owner.packageLocalUnit(state, { env: runEnv }));
        verifyArtifact(artifact);
      } else if (stage === "qualify") {
        if (await check("release") !== 0) throw new Error("release-tier check failed");
        const qualified = await stageEnv(stage, (runEnv) => owner.qualifyLocalUnit(state, { env: runEnv }));
        verifyArtifact(qualified);
        if (!qualified.qualifiedOnly || !qualified.evidencePath || qualified.artifactSha256 !== artifact.artifactSha256) throw new Error("qualification did not bind the packaged artifact");
        artifact = qualified;
      }
      let released = null;
      if (stage === "release") {
        released = await stageEnv(stage, (runEnv) => (releaser ?? releaseUnit)({ unit, root, commit, executor, env: runEnv, artifact, platform, out }));
      }
      report.stages[stage] = { ok: true, ...(stage === "package" || stage === "qualify" ? { artifactFile: artifact.artifactFile, artifactSha256: artifact.artifactSha256 } : {}), ...(released ?? {}) };
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
