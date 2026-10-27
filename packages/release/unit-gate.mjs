// RightKit's own units use the same door, but never reuse mutable unit receipts as evidence.
import { createHash, randomBytes } from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { localBuildGranted } from "./execution-policy.mjs";
import { readCargoPackage } from "./crate-graph.mjs";
import { endRun, startRun, stagesTo } from "./gate-stages.mjs";

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
export async function runUnitGate({ selector, to, root, commonDir, commit, tree, executor, env, platform, check, loadOwner = () => import("./local-publish.mjs"), out = console.log, err = console.error, resolve = resolveUnit, writeReport }) {
  const stages = stagesTo(to);
  if (!stages) { err(`rightkit-gate: invalid unit stage ${to}`); return 2; }
  let unit;
  try { unit = resolve(selector, root); } catch (error) { err(`rightkit-gate: BLOCKED: ${error.message}`); return 1; }
  if (to === "release") { err("rightkit-gate: BLOCKED: unit release requires authoritative qualified-artifact publication; no unit upload was started"); return 1; }
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
      report.stages[stage] = { ok: true, ...(stage === "package" || stage === "qualify" ? { artifactFile: artifact.artifactFile, artifactSha256: artifact.artifactSha256 } : {}) };
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
