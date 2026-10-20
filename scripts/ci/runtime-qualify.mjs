// CI-only runtime evidence. Harness comes from dispatched main; measured source comes from sealed candidate.
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import crypto from "node:crypto";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { archiveTool } from "./registry-consumer.mjs";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
export const CRATE = "rightkit-logs";
export const BENCH_ARGS = Object.freeze(["test", "--locked", "--profile", "release-iterate", "-p", CRATE, "--features", "tracing", "--bench", "budget"]);
export const WORKLOADS = Object.freeze([
  Object.freeze({ id: "tracing", args: BENCH_ARGS, bench: "budget", description: "CI isolated tracing writer: 200000 lines then default idle window" }),
  Object.freeze({ id: "detached", args: Object.freeze([...BENCH_ARGS.slice(0, 7), "tracing,log-bridge", "--bench", "budget_detached"]), bench: "budget_detached", description: "CI isolated detached drainer: 200000 lines then idle at chosen 1000 ms interval; coverage only for caller intervals >=1000 ms, not an API default", intervalMs: 1000 }),
]);
export const REQUIRED_HOSTS = Object.freeze(["win32", "darwin"]);
export const WORKLOAD_PRESCRIPTION_PATH = `crates/${CRATE}/benches/workloads.json`;
export const SUPPORTED_PRESCRIPTION = Object.freeze({
  schema: 1, crate: CRATE, defaultFeatures: true,
  coverage: { default: "http and detached API", tracing: "tracing writer", "log-bridge": "compiled in detached workload; no additional background worker", testing: "test-only; excluded" },
  workloads: WORKLOADS.map((w) => ({ id: w.id, args: [...w.args], bench: w.bench, intervalMs: w.intervalMs ?? null })),
});
export function validateSourcePrescription(value) {
  const canonical = (item) => Array.isArray(item) ? item.map(canonical) : item && typeof item === "object" ? Object.fromEntries(Object.keys(item).sort().map((key) => [key, canonical(item[key])])) : item;
  if (JSON.stringify(canonical(value)) !== JSON.stringify(canonical(SUPPORTED_PRESCRIPTION))) throw new Error("candidate workload prescription differs from trusted supported coverage");
  // Required inventory comes from authenticated candidate bytes, never receipt claims.
  return value.workloads.map((w) => ({ ...w, description: WORKLOADS.find((supported) => supported.id === w.id).description }));
}
export function harnessMetadataPaths(prescription) {
  return [WORKLOAD_PRESCRIPTION_PATH, ...validateSourcePrescription(prescription).map((w) => `crates/${CRATE}/benches/${w.bench}.rs`)];
}
const SHA = /^[a-f0-9]{40}$/;
const hash = (bytes) => crypto.createHash("sha256").update(bytes).digest("hex");
const read = (file) => JSON.parse(fs.readFileSync(file, "utf8"));
const write = (file, value) => fs.writeFileSync(file, `${JSON.stringify(value, null, 2)}\n`);
const MEASURE_ENVIRONMENT = Object.freeze([
  "PATH", "PATHEXT", "COMSPEC", "SystemRoot", "SYSTEMROOT", "WINDIR", "HOME", "USERPROFILE", "TEMP", "TMP", "TMPDIR",
  "GITHUB_ACTIONS", "GITHUB_REF", "GITHUB_SHA", "GITHUB_RUN_ID", "GITHUB_RUN_ATTEMPT", "RUNNER_NAME", "ImageVersion", "RUNTIME_SOURCE", "RUNTIME_PACKED", "RUNTIME_OUT",
]);
export function cleanMeasurementEnvironment(env) {
  return Object.fromEntries(MEASURE_ENVIRONMENT.filter((name) => env[name] !== undefined).map((name) => [name, env[name]]));
}
function command(exe, args, cwd, options = {}) {
  const result = spawnSync(exe, args, { cwd, encoding: "utf8", windowsHide: true, maxBuffer: 64 * 1024 * 1024, ...options });
  if (result.error || result.status !== 0) throw new Error(`${exe} failed: ${result.error?.message ?? result.stderr ?? result.status}`);
  return result.stdout;
}
export function validateRun(run, repository, runId) {
  if (String(run.id) !== runId || run.repository?.full_name !== repository || run.head_repository?.full_name !== repository || run.path !== ".github/workflows/publish.yml" || run.head_branch !== "main" || run.event !== "workflow_dispatch" || run.status !== "completed" || !SHA.test(run.head_sha) || !Number.isSafeInteger(run.run_attempt) || run.run_attempt < 1) throw new Error("candidate must be completed publish.yml dispatch from this repository's main");
  return { repository, runId, runAttempt: String(run.run_attempt), sourceSha: run.head_sha };
}
export function selectArtifact(artifacts) {
  const matches = artifacts.filter((entry) => entry.name === "publish-packed" && !entry.expired);
  if (matches.length !== 1 || !Number.isSafeInteger(matches[0].id) || !/^sha256:[a-f0-9]{64}$/.test(matches[0].digest ?? "")) throw new Error("candidate needs exactly one unexpired publish-packed artifact with SHA-256 digest");
  return matches[0];
}
export function validateManifest(manifest, context, packed) {
  if (manifest.schema !== 1 || manifest.registry !== "crates" || manifest.repository !== context.repository || String(manifest.runId) !== context.runId || String(manifest.runAttempt) !== context.runAttempt || manifest.commit !== context.sourceSha || !SHA.test(manifest.commit)) throw new Error("sealed manifest differs from authenticated candidate run");
  const units = manifest.units?.filter((unit) => unit.name === CRATE) ?? [];
  if (units.length !== 1) throw new Error(`candidate must contain exactly one ${CRATE}`);
  const unit = units[0];
  if (!/^\d+\.\d+\.\d+$/.test(unit.version) || unit.dir !== `crates/${CRATE}` || unit.file !== `${CRATE}-${unit.version}.crate` || !/^[a-f0-9]{64}$/.test(unit.sha256)) throw new Error("candidate crate identity is malformed");
  const bytes = fs.readFileSync(path.join(packed, unit.file));
  if (bytes.length !== unit.size || hash(bytes) !== unit.sha256) throw new Error("sealed crate hash/size mismatch");
  return unit;
}
export function safeArchiveMembers(text) {
  const names = text.trim().split(/\r?\n/);
  if (!names.length || names.some((name) => !name || name.includes("\\") || name.startsWith("/") || name.includes(":") || name.split("/").some((part) => part === ".." || part === "."))) throw new Error("unsafe archive member");
  return names;
}
async function resolveCandidate(env) {
  if (env.GITHUB_ACTIONS !== "true" || env.GITHUB_REF !== "refs/heads/main" || !/^[1-9]\d*$/.test(env.RUNTIME_SOURCE_RUN ?? "")) throw new Error("dispatch from main with numeric source_run");
  const repository = env.GITHUB_REPOSITORY;
  if (!/^[\w.-]+\/[\w.-]+$/.test(repository ?? "")) throw new Error("invalid repository");
  const api = `https://api.github.com/repos/${repository}`;
  const request = async (url) => {
    const result = await fetch(url, { headers: { Authorization: `Bearer ${env.GITHUB_TOKEN}`, Accept: "application/vnd.github+json", "X-GitHub-Api-Version": "2022-11-28" } });
    if (!result.ok) throw new Error(`candidate API HTTP ${result.status}`);
    return result;
  };
  const run = await (await request(`${api}/actions/runs/${env.RUNTIME_SOURCE_RUN}`)).json();
  const context = validateRun(run, repository, env.RUNTIME_SOURCE_RUN);
  const listing = await (await request(`${api}/actions/runs/${context.runId}/artifacts?per_page=100`)).json();
  if (listing.total_count > 100) throw new Error("candidate artifact listing exceeds bound");
  const artifact = selectArtifact(listing.artifacts ?? []);
  if (!Number.isSafeInteger(artifact.size_in_bytes) || artifact.size_in_bytes <= 0 || artifact.size_in_bytes > 512 * 1024 * 1024) throw new Error("candidate archive exceeds 512 MiB bound");
  const bytes = Buffer.from(await (await request(`${api}/actions/artifacts/${artifact.id}/zip`)).arrayBuffer());
  if (bytes.length > 512 * 1024 * 1024) throw new Error("candidate archive exceeds 512 MiB bound");
  if (`sha256:${hash(bytes)}` !== artifact.digest) throw new Error("candidate artifact ZIP digest mismatch");
  const packed = env.RUNTIME_PACKED;
  fs.mkdirSync(packed, { recursive: true });
  const zip = path.join(path.dirname(packed), "runtime-candidate.zip");
  fs.writeFileSync(zip, bytes);
  const names = safeArchiveMembers(command("unzip", ["-Z1", zip], ROOT));
  // publish-packed is deliberately a flat set of files, never directories or links.
  if (new Set(names).size !== names.length || names.some((name) => !/^(?:manifest\.json|[\w.-]+\.(?:crate|upload\.json))$/.test(name))) throw new Error("publish-packed must contain unique flat crate files");
  // Read individual entries as bytes; never materialize ZIP symlinks.
  for (const name of names) fs.writeFileSync(path.join(packed, name), command("unzip", ["-p", zip, name], ROOT, { encoding: "buffer" }));
  Object.assign(context, { artifactId: artifact.id, artifactDigest: artifact.digest, harnessSha: env.GITHUB_SHA });
  validateManifest(read(path.join(packed, "manifest.json")), context, packed);
  write(path.join(packed, "runtime-context.json"), context);
  fs.appendFileSync(env.GITHUB_OUTPUT, `source_sha=${context.sourceSha}\n`);
}
function bindSource(source, packed, env = process.env) {
  const context = read(path.join(packed, "runtime-context.json"));
  const manifestBytes = fs.readFileSync(path.join(packed, "manifest.json"));
  const unit = validateManifest(JSON.parse(manifestBytes), context, packed);
  if (command("git", ["rev-parse", "HEAD"], source).trim() !== context.sourceSha) throw new Error("measured checkout differs from candidate source SHA");
  const archive = path.join(packed, unit.file);
  const tar = archiveTool(process.platform, env);
  const names = safeArchiveMembers(command(tar, ["-tzf", archive], source));
  const prefix = `${CRATE}-${unit.version}/`;
  if (names.some((name) => !name.startsWith(prefix))) throw new Error("unexpected crate archive root");
  const bindings = {};
  const sourcePaths = command("git", ["ls-tree", "-r", "--name-only", context.sourceSha, `${unit.dir}/src`], source).trim().split(/\r?\n/);
  const sealedSources = names.filter((name) => name.startsWith(`${prefix}src/`) && !name.endsWith("/"));
  if (new Set(names).size !== names.length || sealedSources.length !== sourcePaths.length) throw new Error("sealed source inventory differs from candidate");
  const prescription = JSON.parse(command("git", ["cat-file", "blob", `${context.sourceSha}:${WORKLOAD_PRESCRIPTION_PATH}`], source));
  const workloads = validateSourcePrescription(prescription);
  for (const full of [...sourcePaths, `${unit.dir}/Cargo.toml`]) {
    const rel = full.slice(unit.dir.length + 1);
    const member = `${prefix}${rel === "Cargo.toml" ? "Cargo.toml.orig" : rel}`;
    if (!names.includes(member)) throw new Error(`sealed source missing: ${full}`);
    const sealed = command(tar, ["-xOzf", archive, member], source, { encoding: "buffer" });
    const blob = command("git", ["cat-file", "blob", `${context.sourceSha}:${full}`], source, { encoding: "buffer" });
    if (!sealed.equals(blob)) throw new Error(`sealed source differs: ${full}`);
    bindings[full] = hash(blob);
  }
  // Cargo's package include excludes benches/**. Qualification harness metadata is
  // not shipped: bind these exact blobs to authenticated candidate sourceSha only.
  // Missing candidate blobs fail closed; never substitute a newer harness checkout.
  for (const full of harnessMetadataPaths(prescription)) {
    bindings[full] = hash(command("git", ["cat-file", "blob", `${context.sourceSha}:${full}`], source, { encoding: "buffer" }));
  }
  const contextFiles = command("git", ["ls-tree", "-r", "--name-only", context.sourceSha, "Cargo.toml", "Cargo.lock", "rust-toolchain.toml", ".cargo", `${unit.dir}/benches`, "crates/rightkit-child-process/src", "crates/rightkit-child-process/Cargo.toml"], source).trim().split(/\r?\n/).filter(Boolean);
  for (const full of contextFiles) bindings[full] = hash(command("git", ["cat-file", "blob", `${context.sourceSha}:${full}`], source, { encoding: "buffer" }));
  return { context, unit, manifestSha256: hash(manifestBytes), bindings, workloads };
}
export function measurementProblems(record, budget, sourceSha, host) {
  const problems = [];
  if (!record?.host || !record.observed || !record.dataScale) return ["missing measurement"];
  if (record.commit !== sourceSha || record.host.os !== host || !["win32", "darwin"].includes(host) || record.scope !== "synthetic") problems.push("measurement source/host/scope mismatch");
  for (const key of ["residentMemoryBytes", "idleCpuPercent", "wakeupsPerSecond", "diskOpsPerSecond"]) {
    const delta = key === "wakeupsPerSecond" && budget.ceilingBasis?.[key] === "delta-over-bare";
    const value = delta ? record.delta?.[key] : record.observed[key];
    const ceiling = delta ? budget.deltaCeilings?.[key] : budget.ceilings?.[key];
    if (delta && (!Number.isFinite(record.baseline?.[key]) || record.baseline[key] < 0 || !Number.isFinite(record.observed[key]) || Math.abs(record.observed[key] - record.baseline[key] - value) > 0.002)) problems.push(`invalid baseline/delta ${key}`);
    if (!Number.isFinite(ceiling) || ceiling <= 0) problems.push(`undeclared ceiling ${key}`);
    if (value === null && host === "darwin" && key === "diskOpsPerSecond") continue;
    if (!Number.isFinite(value) || (!delta && value < 0) || value > ceiling || (key === "residentMemoryBytes" && value === 0)) problems.push(`measurement exceeds or cannot prove ${key}`);
  }
  if (record.dataScale.unit !== budget.requiredScale.unit || !Number.isFinite(record.dataScale.count) || record.dataScale.count < budget.requiredScale.minCount) problems.push("measurement below required scale");
  return problems;
}
async function measure(env) {
  if (env.GITHUB_ACTIONS !== "true" || env.GITHUB_REF !== "refs/heads/main") throw new Error("CI main only");
  for (const name of Object.keys(env)) if ((/TOKEN|PASSWORD|SECRET|CREDENTIAL/.test(name) && env[name]) || name.startsWith("ACTIONS_ID_TOKEN")) throw new Error(`credential-like variable ${name} forbidden during measurement`);
  const { context, unit, manifestSha256, bindings, workloads } = bindSource(env.RUNTIME_SOURCE, env.RUNTIME_PACKED, env);
  const crateDir = path.join(env.RUNTIME_SOURCE, unit.dir);
  const budget = read(path.join(crateDir, "runtime-budget.json"));
  const original = read(path.join(ROOT, unit.dir, "runtime-budget.json"));
  for (const key of ["ceilings", "requiredScale", "ceilingBasis", "deltaCeilings"]) if (JSON.stringify(budget[key]) !== JSON.stringify(original[key])) throw new Error(`candidate changes frozen ${key}`);
  const output = env.RUNTIME_OUT;
  fs.mkdirSync(output, { recursive: true });
  const { recordMeasurement, measuredPathspecs } = await import("../../packages/release/runtime-budgets.mjs");
  const { specs, problems: sourceProblems = [] } = measuredPathspecs(crateDir);
  if (sourceProblems.length) throw new Error(`cannot bind measured dependency scope: ${sourceProblems.join("; ")}`);
  const measuredPaths = command("git", ["ls-tree", "-r", "--name-only", context.sourceSha, ...specs], env.RUNTIME_SOURCE).trim().split(/\r?\n/).filter(Boolean);
  for (const full of measuredPaths) bindings[full] = hash(command("git", ["cat-file", "blob", `${context.sourceSha}:${full}`], env.RUNTIME_SOURCE, { encoding: "buffer" }));
  const toolchain = { rustc: command("rustc", ["-Vv"], env.RUNTIME_SOURCE).trim(), cargo: command("cargo", ["-V"], env.RUNTIME_SOURCE).trim() };
  const receipts = [];
  // Separate processes, sequential per host; Cargo reuses this checkout's compiled cache.
  for (const workload of workloads) {
    const result = spawnSync("cargo", workload.args, { cwd: env.RUNTIME_SOURCE, env: { ...env, CARGO_INCREMENTAL: "1" }, windowsHide: true, encoding: "utf8", timeout: 40 * 60 * 1000, maxBuffer: 64 * 1024 * 1024 });
    const raw = `${result.stdout ?? ""}\n${result.stderr ?? ""}`;
    const rawFile = `${workload.id}-bench.txt`;
    fs.writeFileSync(path.join(output, rawFile), raw);
    const receipt = { schema: 2, workload: workload.id, intervalMs: workload.intervalMs ?? null, crate: CRATE, version: unit.version, ...context, qualificationRunId: env.GITHUB_RUN_ID, qualificationRunAttempt: env.GITHUB_RUN_ATTEMPT, manifestSha256, bindings, toolchain, host: { os: process.platform, arch: process.arch, release: os.release(), runner: env.RUNNER_NAME, image: env.ImageVersion }, command: `cargo ${workload.args.join(" ")}`, rawFile, rawSha256: hash(Buffer.from(raw)), exitCode: result.status, signal: result.signal, problems: [] };
    if (result.error || result.status !== 0) receipt.problems.push(`bench failed: ${result.error?.message ?? result.status}`);
    else {
      try {
        // Owner recorder deliberately aliases same host/version/scope. Snapshot its validated
        // record immediately, then restore budget history before recording another workload.
        const budgetFile = path.join(crateDir, "runtime-budget.json");
        const before = fs.readFileSync(budgetFile);
        const alias = path.join(crateDir, "runtime-budget.evidence", `${process.platform}-${process.arch}-${unit.version}-synthetic.txt`);
        const priorEvidence = fs.existsSync(alias) ? fs.readFileSync(alias) : null;
        try {
          const record = recordMeasurement({ crateDir, resultFile: path.join(output, rawFile), scope: "synthetic", command: receipt.command, description: workload.description });
          receipt.record = preserveWorkloadRecord(record, workload.id);
          fs.copyFileSync(path.join(crateDir, record.evidence.file), path.join(crateDir, receipt.record.evidence.file));
          fs.copyFileSync(path.join(crateDir, receipt.record.evidence.file), path.join(output, `${workload.id}-evidence.txt`));
        } finally {
          fs.writeFileSync(budgetFile, before);
          if (priorEvidence !== null) fs.writeFileSync(alias, priorEvidence);
          else if (fs.existsSync(alias)) fs.unlinkSync(alias);
        }
        receipt.problems.push(...measurementProblems(receipt.record, budget, context.sourceSha, process.platform));
      } catch (error) { receipt.problems.push(`record rejected: ${error.message}`); }
    }
    receipts.push(receipt);
    write(path.join(output, "qualification.json"), { schema: 2, workloads: receipts });
  }
  const problems = receipts.flatMap((r) => r.problems.map((p) => `${r.workload}: ${p}`));
  if (problems.length) throw new Error(problems.join("; "));
}
export function preserveWorkloadRecord(record, workload) {
  if (!WORKLOADS.some((w) => w.id === workload) || !/^runtime-budget\.evidence\/[\w.-]+\.txt$/.test(record.evidence.file)) throw new Error("invalid workload/evidence path");
  return { ...record, evidence: { ...record.evidence, file: record.evidence.file.replace(/\.txt$/, `-${workload}.txt`) } };
}
export function validateWorkloadReceipts(receipts, context, unit, manifestSha256, bindings, host, harnessSha, budget, qualificationRun) {
  if (!Array.isArray(receipts) || receipts.length !== WORKLOADS.length) throw new Error(`${host}: requires exactly both workloads`);
  const seen = new Set();
  for (const receipt of receipts) {
    const workload = WORKLOADS.find((w) => w.id === receipt.workload);
    if (!workload || seen.has(receipt.workload)) throw new Error("unknown/duplicate workload");
    seen.add(receipt.workload);
    if (qualificationRun && (receipt.qualificationRunId !== qualificationRun.id || receipt.qualificationRunAttempt !== qualificationRun.attempt)) throw new Error("mixed qualification run");
    if (receipt.schema !== 2 || receipt.crate !== CRATE || receipt.version !== unit.version || receipt.host?.os !== host || receipt.exitCode !== 0 || receipt.signal || !Array.isArray(receipt.problems) || receipt.problems.length || receipt.command !== `cargo ${workload.args.join(" ")}` || receipt.intervalMs !== (workload.intervalMs ?? null) || receipt.record?.command !== receipt.command || receipt.record?.crateVersion !== unit.version || receipt.record?.host?.arch !== receipt.host.arch || !receipt.toolchain?.rustc || !receipt.toolchain?.cargo) throw new Error(`${host}: workload identity/status mismatch`);
    for (const key of ["repository", "runId", "runAttempt", "sourceSha", "artifactId", "artifactDigest"]) if (receipt[key] !== context[key]) throw new Error(`${host}: mixed candidate ${key}`);
    if (receipt.manifestSha256 !== manifestSha256 || receipt.harnessSha !== harnessSha || JSON.stringify(Object.entries(receipt.bindings ?? {}).sort()) !== JSON.stringify(Object.entries(bindings).sort())) throw new Error(`${host}: source/manifest/harness binding mismatch`);
    if (receipt.rawFile !== `${workload.id}-bench.txt` || !/^[a-f0-9]{64}$/.test(receipt.rawSha256 ?? "") || receipt.record.evidence.sha256 !== receipt.rawSha256 || !receipt.record.evidence.file.endsWith(`-${workload.id}.txt`)) throw new Error("workload evidence mismatch");
    const problems = measurementProblems(receipt.record, budget, context.sourceSha, host);
    if (problems.length) throw new Error(problems.join("; "));
  }
  if (new Set(receipts.map((r) => r.record.evidence.file)).size !== receipts.length) throw new Error("workload evidence overwrite");
  return receipts;
}

function measureClean(env) {
  const result = spawnSync(process.execPath, [fileURLToPath(import.meta.url), "measure"], {
    cwd: ROOT, env: cleanMeasurementEnvironment(env), stdio: "inherit", windowsHide: true,
  });
  if (result.error || result.status !== 0) throw new Error(`clean measurement process failed: ${result.error?.message ?? result.status}`);
}
async function combine(env) {
  const { context, unit, manifestSha256, bindings, workloads } = bindSource(env.RUNTIME_SOURCE, env.RUNTIME_PACKED, env);
  const crateDir = path.join(env.RUNTIME_SOURCE, unit.dir);
  const budget = read(path.join(crateDir, "runtime-budget.json"));
  const records = [];
  const proofFiles = {};
  fs.mkdirSync(env.RUNTIME_OUT, { recursive: true });
  // Match dependency binding inventory used by each producer.
  const { measuredPathspecs } = await import("../../packages/release/runtime-budgets.mjs");
  const { specs, problems = [] } = measuredPathspecs(crateDir);
  if (problems.length) throw new Error(problems.join("; "));
  for (const full of command("git", ["ls-tree", "-r", "--name-only", context.sourceSha, ...specs], env.RUNTIME_SOURCE).trim().split(/\r?\n/).filter(Boolean)) bindings[full] = hash(command("git", ["cat-file", "blob", `${context.sourceSha}:${full}`], env.RUNTIME_SOURCE, { encoding: "buffer" }));
  for (const host of REQUIRED_HOSTS) {
    const dir = path.join(env.RUNTIME_RESULTS, `runtime-${host}`);
    const envelope = read(path.join(dir, "qualification.json"));
    if (envelope.schema !== 2) throw new Error("legacy single-workload evidence is insufficient");
    for (const receipt of validateWorkloadReceipts(envelope.workloads, context, unit, manifestSha256, bindings, host, env.GITHUB_SHA, budget, { id: env.GITHUB_RUN_ID, attempt: env.GITHUB_RUN_ATTEMPT })) {
      const bytes = fs.readFileSync(path.join(dir, `${receipt.workload}-evidence.txt`));
      if (hash(bytes) !== receipt.record.evidence.sha256 || hash(fs.readFileSync(path.join(dir, receipt.rawFile))) !== receipt.rawSha256 || !/^runtime-budget\.evidence\/[\w.-]+\.txt$/.test(receipt.record.evidence.file)) throw new Error("measurement evidence mismatch");
      fs.mkdirSync(path.join(crateDir, "runtime-budget.evidence"), { recursive: true });
      fs.writeFileSync(path.join(crateDir, receipt.record.evidence.file), bytes);
      records.push(receipt.record);
    }
    // Preserve complete producer envelopes & raw outputs for downstream authentication.
    for (const name of ["qualification.json", ...workloads.flatMap((w) => [`${w.id}-bench.txt`, `${w.id}-evidence.txt`])]) {
      const relative = `${host}/${name}`;
      const bytes = fs.readFileSync(path.join(dir, name));
      fs.mkdirSync(path.join(env.RUNTIME_OUT, host), { recursive: true });
      fs.writeFileSync(path.join(env.RUNTIME_OUT, relative), bytes);
      proofFiles[relative] = { sha256: hash(bytes), size: bytes.length };
    }
  }
  // Replace only this version's synthetic runs on qualified hosts; retain other history.
  budget.measurements = [...budget.measurements.filter((m) => !(m.crateVersion === unit.version && m.scope === "synthetic" && ["win32", "darwin"].includes(m.host.os))), ...records];
  write(path.join(crateDir, "runtime-budget.json"), budget);
  const { syncRuntimeBudgetReadme, evaluateRuntimeBudget } = await import("../../packages/release/runtime-budgets.mjs");
  const readme = path.join(crateDir, "README.md");
  fs.writeFileSync(readme, syncRuntimeBudgetReadme(fs.readFileSync(readme, "utf8"), budget));
  const verdict = evaluateRuntimeBudget({ crate: CRATE, crateDir, version: unit.version });
  fs.mkdirSync(env.RUNTIME_OUT, { recursive: true });
  for (const name of ["runtime-budget.json", "README.md"]) fs.copyFileSync(path.join(crateDir, name), path.join(env.RUNTIME_OUT, name));
  fs.cpSync(path.join(crateDir, "runtime-budget.evidence"), path.join(env.RUNTIME_OUT, "runtime-budget.evidence"), { recursive: true });
  write(path.join(env.RUNTIME_OUT, "qualification.json"), { schema: 2, ...context, manifestSha256, workloads: workloads.map((w) => w.id), proofFiles, records, verdict });
  if (!verdict.ok) throw new Error(verdict.problems.join("; "));
}
if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const action = { resolve: resolveCandidate, measure, "measure-clean": measureClean, combine }[process.argv[2]];
    if (!action) throw new Error("usage: runtime-qualify.mjs resolve|measure|measure-clean|combine");
    await action(process.env);
  } catch (error) { console.error(error.message); process.exitCode = 1; }
}
