// CI-only runtime evidence. Harness comes from dispatched main; measured source comes from sealed candidate.
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import crypto from "node:crypto";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
export const CRATE = "rightkit-logs";
export const BENCH_ARGS = Object.freeze(["test", "--locked", "--profile", "release-iterate", "-p", CRATE, "--features", "tracing", "--bench", "budget"]);
const SHA = /^[a-f0-9]{40}$/;
const hash = (bytes) => crypto.createHash("sha256").update(bytes).digest("hex");
const read = (file) => JSON.parse(fs.readFileSync(file, "utf8"));
const write = (file, value) => fs.writeFileSync(file, `${JSON.stringify(value, null, 2)}\n`);
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
function bindSource(source, packed) {
  const context = read(path.join(packed, "runtime-context.json"));
  const manifestBytes = fs.readFileSync(path.join(packed, "manifest.json"));
  const unit = validateManifest(JSON.parse(manifestBytes), context, packed);
  if (command("git", ["rev-parse", "HEAD"], source).trim() !== context.sourceSha) throw new Error("measured checkout differs from candidate source SHA");
  const archive = path.join(packed, unit.file);
  const names = safeArchiveMembers(command("tar", ["-tzf", archive], source));
  const prefix = `${CRATE}-${unit.version}/`;
  if (names.some((name) => !name.startsWith(prefix))) throw new Error("unexpected crate archive root");
  const bindings = {};
  const sourcePaths = command("git", ["ls-tree", "-r", "--name-only", context.sourceSha, `${unit.dir}/src`], source).trim().split(/\r?\n/);
  const sealedSources = names.filter((name) => name.startsWith(`${prefix}src/`) && !name.endsWith("/"));
  if (new Set(names).size !== names.length || sealedSources.length !== sourcePaths.length) throw new Error("sealed source inventory differs from candidate");
  for (const full of [...sourcePaths, `${unit.dir}/Cargo.toml`]) {
    const rel = full.slice(unit.dir.length + 1);
    const member = `${prefix}${rel === "Cargo.toml" ? "Cargo.toml.orig" : rel}`;
    if (!names.includes(member)) throw new Error(`sealed source missing: ${full}`);
    const sealed = command("tar", ["-xOzf", archive, member], source, { encoding: "buffer" });
    const blob = command("git", ["cat-file", "blob", `${context.sourceSha}:${full}`], source, { encoding: "buffer" });
    if (!sealed.equals(blob)) throw new Error(`sealed source differs: ${full}`);
    bindings[full] = hash(blob);
  }
  const contextFiles = command("git", ["ls-tree", "-r", "--name-only", context.sourceSha, "Cargo.toml", "Cargo.lock", "rust-toolchain.toml", ".cargo", `${unit.dir}/benches`, "crates/rightkit-child-process/src", "crates/rightkit-child-process/Cargo.toml"], source).trim().split(/\r?\n/).filter(Boolean);
  for (const full of contextFiles) bindings[full] = hash(command("git", ["cat-file", "blob", `${context.sourceSha}:${full}`], source, { encoding: "buffer" }));
  return { context, unit, manifestSha256: hash(manifestBytes), bindings };
}
export function measurementProblems(record, budget, sourceSha, host) {
  const problems = [];
  if (record.commit !== sourceSha || record.host.os !== host || !["win32", "darwin"].includes(host) || record.scope !== "synthetic") problems.push("measurement source/host/scope mismatch");
  for (const key of ["residentMemoryBytes", "idleCpuPercent", "wakeupsPerSecond", "diskOpsPerSecond"]) {
    const value = record.observed[key];
    if (!Number.isFinite(budget.ceilings?.[key]) || budget.ceilings[key] <= 0) problems.push(`undeclared ceiling ${key}`);
    if (value === null && host === "darwin" && key === "diskOpsPerSecond") continue;
    if (!Number.isFinite(value) || value < 0 || value > budget.ceilings[key] || (key === "residentMemoryBytes" && value === 0)) problems.push(`measurement exceeds or cannot prove ${key}`);
  }
  if (record.dataScale.unit !== budget.requiredScale.unit || record.dataScale.count < budget.requiredScale.minCount) problems.push("measurement below required scale");
  return problems;
}
async function measure(env) {
  if (env.GITHUB_ACTIONS !== "true" || env.GITHUB_REF !== "refs/heads/main") throw new Error("CI main only");
  for (const name of Object.keys(env)) if ((/TOKEN|PASSWORD|SECRET|CREDENTIAL/.test(name) && env[name]) || name.startsWith("ACTIONS_ID_TOKEN")) throw new Error(`credential-like variable ${name} forbidden during measurement`);
  const { context, unit, manifestSha256, bindings } = bindSource(env.RUNTIME_SOURCE, env.RUNTIME_PACKED);
  const crateDir = path.join(env.RUNTIME_SOURCE, unit.dir);
  const budget = read(path.join(crateDir, "runtime-budget.json"));
  const original = read(path.join(ROOT, unit.dir, "runtime-budget.json"));
  for (const key of ["ceilings", "requiredScale", "ceilingBasis", "deltaCeilings"]) if (JSON.stringify(budget[key]) !== JSON.stringify(original[key])) throw new Error(`candidate changes frozen ${key}`);
  const output = env.RUNTIME_OUT;
  fs.mkdirSync(output, { recursive: true });
  const { recordMeasurement, measuredPathspecs } = await import("../../packages/release/runtime-budgets.mjs");
  const { specs } = measuredPathspecs(crateDir);
  const measuredPaths = command("git", ["ls-tree", "-r", "--name-only", context.sourceSha, ...specs], env.RUNTIME_SOURCE).trim().split(/\r?\n/).filter(Boolean);
  for (const full of measuredPaths) bindings[full] = hash(command("git", ["cat-file", "blob", `${context.sourceSha}:${full}`], env.RUNTIME_SOURCE, { encoding: "buffer" }));
  const toolchain = { rustc: command("rustc", ["-Vv"], env.RUNTIME_SOURCE).trim(), cargo: command("cargo", ["-V"], env.RUNTIME_SOURCE).trim() };
  const result = spawnSync("cargo", BENCH_ARGS, { cwd: env.RUNTIME_SOURCE, env: { ...env, CARGO_INCREMENTAL: "1" }, windowsHide: true, encoding: "utf8", timeout: 40 * 60 * 1000, maxBuffer: 64 * 1024 * 1024 });
  const raw = `${result.stdout ?? ""}\n${result.stderr ?? ""}`;
  fs.writeFileSync(path.join(output, "bench.txt"), raw);
  const receipt = { schema: 1, crate: CRATE, version: unit.version, ...context, manifestSha256, bindings, toolchain, host: { os: process.platform, arch: process.arch, release: os.release(), runner: env.RUNNER_NAME, image: env.ImageVersion }, command: `cargo ${BENCH_ARGS.join(" ")}`, exitCode: result.status, problems: [] };
  if (result.error || result.status !== 0) receipt.problems.push(`bench failed: ${result.error?.message ?? result.status}`);
  else {
    try {
      receipt.record = recordMeasurement({ crateDir, resultFile: path.join(output, "bench.txt"), scope: "synthetic", command: receipt.command, description: "CI isolated tracing writer: 200000 lines then default idle window" });
      receipt.problems.push(...measurementProblems(receipt.record, budget, context.sourceSha, process.platform));
      fs.copyFileSync(path.join(crateDir, receipt.record.evidence.file), path.join(output, "evidence.txt"));
    } catch (error) { receipt.problems.push(`record rejected: ${error.message}`); }
  }
  write(path.join(output, "qualification.json"), receipt);
  if (receipt.problems.length) throw new Error(receipt.problems.join("; "));
}
async function combine(env) {
  const { context, unit, manifestSha256 } = bindSource(env.RUNTIME_SOURCE, env.RUNTIME_PACKED);
  const crateDir = path.join(env.RUNTIME_SOURCE, unit.dir);
  const budget = read(path.join(crateDir, "runtime-budget.json"));
  const records = [];
  for (const host of ["win32", "darwin"]) {
    const dir = path.join(env.RUNTIME_RESULTS, `runtime-${host}`);
    const receipt = read(path.join(dir, "qualification.json"));
    if (receipt.problems.length || receipt.sourceSha !== context.sourceSha || receipt.manifestSha256 !== manifestSha256 || receipt.harnessSha !== env.GITHUB_SHA || receipt.artifactId !== context.artifactId || receipt.artifactDigest !== context.artifactDigest || receipt.record?.crateVersion !== unit.version) throw new Error(`${host}: qualification receipt mismatch/failure`);
    const problems = measurementProblems(receipt.record, budget, context.sourceSha, host);
    if (problems.length) throw new Error(problems.join("; "));
    const bytes = fs.readFileSync(path.join(dir, "evidence.txt"));
    if (hash(bytes) !== receipt.record.evidence.sha256 || !/^runtime-budget\.evidence\/[\w.-]+\.txt$/.test(receipt.record.evidence.file)) throw new Error("measurement evidence mismatch");
    fs.mkdirSync(path.join(crateDir, "runtime-budget.evidence"), { recursive: true });
    fs.writeFileSync(path.join(crateDir, receipt.record.evidence.file), bytes);
    records.push(receipt.record);
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
  write(path.join(env.RUNTIME_OUT, "qualification.json"), { schema: 1, ...context, manifestSha256, verdict });
  if (!verdict.ok) throw new Error(verdict.problems.join("; "));
}
if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const action = { resolve: resolveCandidate, measure, combine }[process.argv[2]];
    if (!action) throw new Error("usage: runtime-qualify.mjs resolve|measure|combine");
    await action(process.env);
  } catch (error) { console.error(error.message); process.exitCode = 1; }
}
