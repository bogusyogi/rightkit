// Secretless admission only. Candidate code is data, never imported or executed.
import fs from "node:fs";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { sha, REPOSITORY, localRuntimeProofSha256 } from "./upload.mjs";
import { WINDOW_POLICY, windowProblems, worstIndex } from "./local-runtime-proof.mjs";
import { validateSupplementPin, candidateHash, retainedSupplementOriginal, authenticateSupplement } from "./supplemental-proof.mjs";
import { parseYaml } from "./workflow-yaml.mjs";
import { fileURLToPath } from "node:url";
import { parse as parseModule } from "es-module-lexer/js";
import { assertFrozenBudget } from "./runtime-qualify.mjs";
import { BUDGET_FIGURES, parseBudgetResult, evaluateRuntimeBudget, syncRuntimeBudgetReadme } from "../../packages/release/runtime-budgets.mjs";
export const RUNTIME_WORKFLOW = ".github/workflows/qualify-runtime.yml";
const authenticated = new WeakMap();
const need = (ok, why) => { if (!ok) throw new Error(`runtime supplement refused: ${why}`); };
const hash = (bytes) => sha("sha256", bytes);
const same = (a, b) => JSON.stringify(a) === JSON.stringify(b);
const safe = (name) => typeof name === "string" && /^[A-Za-z0-9_.-]+(?:\/[A-Za-z0-9_.-]+)*$/.test(name) && name.split("/").every((p) => p !== "." && p !== "..");
const TRUST_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
// This source has one computed loader with exactly two fixed call sites. Pin
// every byte as well as its expression so a new call cannot extend this closure.
const COMPUTED_HELPER_IMPORTS = Object.freeze({
  "packages/release/local-publish.mjs": Object.freeze({
    sha256: "ee715dd020151c8f975016836a185277056d4e78b6cc8aa830bf2f52a7e9cb3c",
    expression: 'pathToFileURL(path.join(workspaceRoot, "scripts", "ci", file)).href',
    count: 1,
    paths: Object.freeze(["scripts/ci/test-results.mjs", "scripts/ci/upload.mjs"]),
  }),
});
/** Relative helper closure is discovered from trusted files, never producer imports. */
export function trustedRuntimeHelperPaths() {
  const pending = ["packages/release/runtime-budgets.mjs", "scripts/ci/runtime-qualify.mjs"], seen = new Set();
  while (pending.length) {
    const name = pending.pop();
    if (seen.has(name)) continue;
    need(safe(name) && seen.size < 128, "helper closure exceeds supported boundary");
    seen.add(name);
    const text = fs.readFileSync(path.join(TRUST_ROOT, name), "utf8");
    const computed = COMPUTED_HELPER_IMPORTS[name];
    if (computed) need(hash(Buffer.from(text)) === computed.sha256, `computed helper source differs from trusted descriptor: ${name}`);
    let computedCount = 0;
    // Parse source syntax only: comments & template text may contain generated
    // imports, while template interpolations can contain real executable imports.
    const [imports] = parseModule(text, name);
    for (const specifier of imports) {
      if (specifier.d === -2) continue; // import.meta has no dependency.
      if (typeof specifier.n !== "string") {
        need(computed && text.slice(specifier.s, specifier.e) === computed.expression && ++computedCount <= computed.count, `computed helper import cannot prove closure: ${name}`);
        pending.push(...computed.paths);
        continue;
      }
      if (!specifier.n.startsWith(".")) continue;
      const relative = path.posix.normalize(path.posix.join(path.posix.dirname(name), specifier.n));
      need(safe(relative), "helper import escapes trusted source");
      pending.push(relative);
    }
    if (computed) need(computedCount === computed.count, `computed helper import count differs from trusted descriptor: ${name}`);
  }
  return [...seen, "pnpm-lock.yaml", "package.json", "packages/release/package.json"].sort();
}
export function validateFrozenRuntimeBudget(budget, trusted) {
  // Top-level ceilings & scale, and each declared profile's features, ceilings & scale (runtime-qualify.mjs frozenBudgetView).
  try { assertFrozenBudget(budget, trusted); } catch (error) { need(false, error.message.replace("candidate changes", "candidate changes trusted")); }
}
/** Reparse raw output, then run full current owner gate against an immutable source overlay. */
export function validateRuntimeRecords({ source, sourceSha, unit, budget, records, rawByEvidence, profiles = new Map() }) {
  // `profiles` maps a record's evidence file to the budget profile that governs it (trusted workload table).
  // Those records are held to their profile by the receipt check; the top-level owner gate sees only the rest.
  const topRecords = records.filter((record) => !profiles.has(record.evidence?.file));
  const profileBudgets = budget.profiles ? structuredClone(budget.profiles) : undefined;
  for (const record of records) {
    const name = profiles.get(record.evidence?.file);
    if (name !== undefined) { need(profileBudgets?.[name], "record names an undeclared budget profile"); profileBudgets[name].measurements = [...(profileBudgets[name].measurements ?? []), record]; }
  }
  for (const record of records) {
    const raw = rawByEvidence.get(record.evidence?.file);
    need(Buffer.isBuffer(raw) && hash(raw) === record.evidence.sha256, "missing raw record evidence");
    const parsed = parseBudgetResult(raw.toString("utf8"));
    need(parsed.ok, `owner raw parser rejected: ${parsed.problems?.join("; ")}`);
    const value = parsed.value;
    need(value.crate === unit.name && value.version === unit.version && record.crateVersion === unit.version && record.commit === sourceSha && record.scope === "synthetic", "raw record source/version/scope mismatch");
    need(BUDGET_FIGURES.every((key) => value[key] === record.observed?.[key]) && value.scale_unit === record.dataScale?.unit && value.scaleCount === record.dataScale?.count && value.bareWakeupsPerSecond === record.baseline?.wakeupsPerSecond && value.wakeupsDeltaPerSecond === record.delta?.wakeupsPerSecond, "raw RESULT differs from receipt record");
  }
  const crateDir = path.join(source.root, "crates", unit.name);
  const overlay = new Map([[path.join(crateDir, "runtime-budget.json"), Buffer.from(JSON.stringify({ ...budget, ...(profileBudgets ? { profiles: profileBudgets } : {}), measurements: topRecords }))]]);
  const readSource = (name) => Buffer.from(source.readData(name));
  overlay.set(path.join(crateDir, "README.md"), Buffer.from(syncRuntimeBudgetReadme(readSource(`crates/${unit.name}/README.md`).toString("utf8"), { ...budget, measurements: topRecords })));
  for (const [name, bytes] of rawByEvidence) {
    need(/^runtime-budget\.evidence\/[\w.-]+\.txt$/.test(name), "unsafe record evidence path");
    overlay.set(path.join(crateDir, name), bytes);
  }
  const readFile = (absolute, encoding) => {
    const relative = path.relative(source.root, absolute).split(path.sep).join("/");
    need(safe(relative), "owner source path escapes snapshot");
    const bytes = overlay.get(absolute) ?? readSource(relative);
    return encoding ? bytes.toString(encoding) : bytes;
  };
  const exists = (absolute) => { try { readFile(absolute); return true; } catch { return false; } };
  // readData is an authenticated immutable tree, so owner freshness reads describe
  // this exact SHA snapshot instead of unrelated validator working-tree changes.
  const git = (args) => {
    if (args[0] === "cat-file" && args[1] === "-t" && args[2] === sourceSha) return { status: 0, stdout: "commit" };
    if (args[0] === "merge-base" && args[1] === "--is-ancestor" && args[2] === sourceSha) return { status: 0, stdout: "" };
    if (args[0] === "diff" && args[2] === sourceSha || args[0] === "ls-files") return { status: 0, stdout: "" };
    if (args[0] === "show" && args[1] === `${sourceSha}:./Cargo.lock`) { try { return { status: 0, stdout: readSource("Cargo.lock").toString("utf8") }; } catch { return { status: 1, stdout: "" }; } }
    if (args[0] === "ls-tree" && args[1] === sourceSha && !exists(path.join(source.root, "Cargo.lock"))) return { status: 0, stdout: "" };
    return { status: 1, stdout: "unsupported immutable snapshot query" };
  };
  const verdict = evaluateRuntimeBudget({ crate: unit.name, crateDir, version: unit.version }, { readFile, exists, git });
  need(verdict.ok, `trusted owner record gate rejected: ${verdict.problems.join("; ")}`);
  return records;
}
function command(spawn, file, args, cwd) {
  const result = spawn(file, args, { cwd, encoding: "buffer", windowsHide: true, shell: false, timeout: 30000, maxBuffer: 8 * 1024 * 1024 });
  need(!result.error && !result.signal && result.status === 0, "data extraction failed");
  return Buffer.from(result.stdout ?? "");
}
export function validateRuntimeWorkflow(text) {
  const workflow = parseYaml(text);
  const measure = workflow.jobs?.measure;
  need(measure && measure.needs === "candidate" && !measure.permissions && !measure.env && !workflow.env, "unsupported measurement job authority");
  need(workflow.permissions?.contents === "read" && workflow.permissions?.actions === "read" && Object.keys(workflow.permissions).length === 2, "unexpected workflow permissions");
  const matrix = measure.strategy?.matrix?.include;
  need(same(matrix, [{ runner: "windows-2025", host: "win32" }, { runner: "xcode-27", host: "darwin" }]), "unsupported native job names");
  need(Array.isArray(measure.steps), "missing measurement steps");
  for (const step of measure.steps) {
    need((!step.if || (step.uses?.startsWith("actions/upload-artifact@") && step.if === "${{ always() }}")) && !step.continueOnError && !step["continue-on-error"], "conditional measurement step");
    const content = JSON.stringify(step);
    need(!/secrets\.|github\.token|id-token|GITHUB_TOKEN|NODE_AUTH_TOKEN|CARGO_REGISTRY_TOKEN|ACTIONS_ID_TOKEN|PASSWORD/i.test(content), "credential-bearing measure workflow");
    if (step.uses) need([
      "actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1",
      "actions/setup-node@820762786026740c76f36085b0efc47a31fe5020",
      "pnpm/action-setup@0977fd99725f1db4007ccb2928dbb4e90d06cc86",
      "dtolnay/rust-toolchain@f8be11a05b1d4f3fcebe6410cc16743212b999b0",
      "actions/download-artifact@d3f86a106a0bac45b974a628896c90dbdf5c8093",
      "actions/upload-artifact@ea165f8d65b6e75b540449e92b4886f43607fa02",
    ].includes(step.uses), "unsupported measure action");
    if (step.uses?.startsWith("actions/checkout@")) need(step.with?.["persist-credentials"] === "false", "checkout retains credentials");
  }
  const runs = measure.steps.filter((s) => s.run).map((s) => s.run);
  need(same(runs, ["pnpm install --frozen-lockfile --ignore-scripts", "node harness/scripts/ci/runtime-qualify.mjs measure-clean"]), "unsupported measure commands");
  const upload = workflow.jobs?.qualify?.steps?.filter((s) => s.uses?.startsWith("actions/upload-artifact@") && s.with?.name === "runtime-qualified");
  need(upload?.length === 1 && upload[0].with["retention-days"] === "90", "runtime-qualified requires 90-day retention");
  return workflow;
}
/** No candidate module loads; producer source & original sealed bytes are checked again. */
export async function verifyRuntimeSupplement({ candidate, pin, manifest, manifestSha256, source, packDir, env, fetchImpl = fetch, spawn = spawnSync }) {
  need(!Object.entries(env ?? {}).some(([key, value]) => value && (/NODE_AUTH_TOKEN|CARGO_REGISTRY_TOKEN|ACTIONS_ID_TOKEN|PASSWORD|SECRET|CREDENTIAL/i.test(key))), "secretless admission required");
  validateSupplementPin(pin, candidate);
  need(pin.kind === "runtime-workload-pin" && candidate.registry === "crates" && source?.source?.sha === candidate.sourceSha && path.isAbsolute(source.root), "runtime candidate/source mismatch");
  need(manifestSha256 === candidate.manifestSha256, "caller manifest hash mismatch");
  const root = fs.mkdtempSync(path.join(env.RUNNER_TEMP, "runtime-supplement-"));
  try {
    const original = await retainedSupplementOriginal({ candidate, env, fetchImpl, root, spawn });
    need(same(original.manifest, manifest), "caller manifest differs from retained original");
    const jobs = pin.producer.jobs;
    const { destination, api } = await authenticateSupplement({ candidate, pin, env, fetchImpl, spawn, root, repository: original.repository, workflowPath: RUNTIME_WORKFLOW, requiredJobs: { candidate: jobs.candidate, "measure (windows-2025, win32)": jobs.win32, "measure (xcode-27, darwin)": jobs.darwin, qualify: jobs.qualify }, name: "runtime-qualified" });
    const blob = async (name) => {
      const result = await api(`contents/${name}?ref=${pin.producer.sourceSha}`);
      need(result.encoding === "base64" && typeof result.content === "string", "missing producer blob");
      const bytes = Buffer.from(result.content, "base64");
      need(result.sha === sha("sha1", Buffer.concat([Buffer.from(`blob ${bytes.length}\0`), bytes])), "producer blob hash mismatch");
      return bytes;
    };
    const workflowBytes = await blob(RUNTIME_WORKFLOW);
    validateRuntimeWorkflow(workflowBytes.toString("utf8"));
    need(hash(workflowBytes) === hash(fs.readFileSync(new URL("../../.github/workflows/qualify-runtime.yml", import.meta.url))), "producer workflow differs from trusted supported workflow");
    // Runtime helper is trusted admission code, never a module from source.root.
    const owner = await import("./runtime-qualify.mjs");
    const harness = await blob("scripts/ci/runtime-qualify.mjs");
    need(hash(harness) === hash(fs.readFileSync(new URL("./runtime-qualify.mjs", import.meta.url))), "producer harness differs from trusted supported harness");
    for (const name of trustedRuntimeHelperPaths()) need(hash(await blob(name)) === hash(fs.readFileSync(path.join(TRUST_ROOT, name))), `producer owner helper differs from trusted supported helper: ${name}`);
    need(typeof source.readData === "function", "authenticated source reader required");
    const readSource = (name) => { need(safe(name), "unsafe source path"); return Buffer.from(source.readData(name)); };
    const raw = fs.readFileSync(path.join(destination, "qualification.json"));
    need(hash(raw) === pin.artifact.proofSha256, "raw runtime proof mismatch");
    const proof = JSON.parse(raw);
    // The authenticated proof names the one crate it measured; that crate must be a sealed candidate unit.
    need(owner.isCrateName(proof.crate), "proof crate name invalid");
    const crate = proof.crate;
    need(Array.isArray(candidate.units) && candidate.units.includes(crate), "proof crate is not a candidate unit");
    const units = manifest.units.filter((u) => u.name === crate);
    need(units.length === 1, "proof crate absent from sealed manifest");
    const unit = units[0];
    need(unit.file === `${crate}-${unit.version}.crate`, "malformed sealed crate identity");
    const archive = fs.readFileSync(path.join(original.packDir, unit.file));
    need(hash(archive) === unit.sha256 && archive.length === unit.size, "original archive mismatch");
    if (packDir) need(hash(fs.readFileSync(path.join(packDir, unit.file))) === unit.sha256, "caller archive mismatch");
    let prescribed;
    try { prescribed = owner.readSourcePrescription(crate, readSource); } catch (error) { need(false, error.message); }
    const workloads = prescribed.workloads;
    const context = { repository: env.GITHUB_REPOSITORY, runId: candidate.runId, runAttempt: candidate.runAttempt, sourceSha: candidate.sourceSha, artifactId: Number(candidate.artifacts.packed.id), artifactDigest: candidate.artifacts.packed.digest, crate };
    need(proof.schema === 2 && proof.manifestSha256 === manifestSha256 && Object.entries(context).every(([k,v]) => proof[k] === v) && proof.harnessSha === pin.producer.sourceSha && same(proof.workloads, workloads.map((w) => w.id)), "combined runtime identity mismatch");
    let budget;
    try { budget = JSON.parse(readSource(`crates/${crate}/runtime-budget.json`)); } catch { need(false, "candidate runtime budget missing or not JSON"); }
    let trustedBudget;
    try { trustedBudget = JSON.parse(fs.readFileSync(path.join(TRUST_ROOT, `crates/${crate}/runtime-budget.json`), "utf8")); } catch { need(false, "trusted runtime budget missing"); }
    validateFrozenRuntimeBudget(budget, trustedBudget);
    let bindings;
    const reports = [];
    const rawByEvidence = new Map();
    const profileByEvidence = new Map();
    const trustedWorkloads = owner.workloadsFor(crate);
    for (const host of owner.REQUIRED_HOSTS) {
      const envelope = JSON.parse(fs.readFileSync(path.join(destination, host, "qualification.json")));
      need(envelope.schema === 2 && Array.isArray(envelope.workloads), "missing raw host envelope");
      const current = envelope.workloads[0]?.bindings;
      need(current && Object.keys(current).length && (!bindings || same(bindings, current)), "mixed host source bindings");
      bindings = current;
      for (const [name, digest] of Object.entries(bindings)) {
        need(hash(readSource(name)) === digest, "source binding differs from original candidate");
        if (name.startsWith(`crates/${crate}/src/`) || name === `crates/${crate}/Cargo.toml`) {
          const rel = name.slice(`crates/${crate}/`.length);
          const member = `${crate}-${unit.version}/${rel === "Cargo.toml" ? "Cargo.toml.orig" : rel}`;
          need(hash(command(spawn, "tar", ["-xOzf", path.join(original.packDir, unit.file), member], source.root)) === digest, "sealed source/bench binding mismatch");
        }
      }
      for (const name of [owner.workloadPrescriptionPath(crate), ...owner.trustedRequiredSources(crate), `crates/${crate}/Cargo.toml`, ...workloads.map((w) => `crates/${crate}/benches/${w.bench}.rs`)]) need(Object.hasOwn(bindings, name), "required sealed binding absent");
      owner.validateWorkloadReceipts(envelope.workloads, context, unit, manifestSha256, bindings, host, pin.producer.sourceSha, budget, { id: pin.producer.runId, attempt: pin.producer.runAttempt });
      for (const receipt of envelope.workloads) {
        for (const name of [`${receipt.workload}-bench.txt`, `${receipt.workload}-evidence.txt`]) {
          const relative = `${host}/${name}`;
          const bytes = fs.readFileSync(path.join(destination, relative));
          need(hash(bytes) === receipt.rawSha256 && proof.proofFiles?.[relative]?.sha256 === hash(bytes) && proof.proofFiles[relative].size === bytes.length, "raw workload evidence mismatch");
          if (name === `${receipt.workload}-bench.txt`) {
            need(!rawByEvidence.has(receipt.record.evidence.file), "duplicate raw record evidence path");
            rawByEvidence.set(receipt.record.evidence.file, bytes);
          }
        }
        reports.push(structuredClone(receipt.record));
        const profile = trustedWorkloads.find((w) => w.id === receipt.workload)?.profile;
        if (profile) profileByEvidence.set(receipt.record.evidence.file, profile.name);
      }
      const relative = `${host}/qualification.json`;
      const bytes = fs.readFileSync(path.join(destination, relative));
      need(proof.proofFiles?.[relative]?.sha256 === hash(bytes) && proof.proofFiles[relative].size === bytes.length, "raw host envelope hash mismatch");
    }
    validateRuntimeRecords({ source, sourceSha: candidate.sourceSha, unit, budget, records: reports, rawByEvidence, profiles: profileByEvidence });
    const handle = Object.freeze({ candidateId: candidate.id, proofSha256: pin.artifact.proofSha256 });
    authenticated.set(handle, { candidate: candidateHash(candidate), unit: { name: unit.name, version: unit.version, sha256: unit.sha256 }, reports });
    return handle;
  } finally { fs.rmSync(root, { recursive: true, force: true }); }
}
const LOCAL_HOST_KEYS = Object.freeze({ win32: "windows", darwin: "macos" });
/**
 * Admits the runtime proof of one runtime owner of a LOCAL candidate (source "local"): the proof each native host wrote into
 * its pinned `local-verify` receipt after measuring on its own broker (packages/release/local-runtime.mjs). Nothing the
 * candidate ships runs; the receipts are DATA. Every check the Actions path applies to a runtime proof applies here, through
 * the same validators (workload table, prescription equality, frozen budget, validateWorkloadReceipts, the owner record gate):
 *   - the workloads, their order, arguments, env names and profiles are the TRUSTED table's, never the receipt's or the candidate's;
 *   - the budget's ceilings, scale and profiles equal the trusted checkout's (a loosened profile or short scale refuses);
 *   - every source binding hashes to the candidate's blob, and the harness helper closure equals the promotion checkout's;
 *   - every repeat of every workload passes the owner gate at full scale on both hosts, in a quiet window recomputed from
 *     its samples (local-runtime-proof.mjs), and the submitted record is the worst repeat.
 * Returns the opaque handle runtimeFeatureProofPolicy accepts; a caller's JSON never qualifies a unit.
 */
export async function verifyLocalRuntimeProof({ candidate, manifest, manifestSha256, unit, source, receipts }) {
  need(candidate?.source === "local" && candidate.registry === "crates" && Array.isArray(candidate.units) && candidate.units.includes(unit?.name), "local runtime proof is for a unit of a local crates candidate");
  need(source?.source?.sha === candidate.sourceSha && path.isAbsolute(source.root ?? "") && typeof source.readData === "function", "local runtime candidate/source mismatch");
  need(manifest?.commit === candidate.sourceSha && /^[0-9a-f]{64}$/.test(manifestSha256 ?? ""), "local runtime manifest binding mismatch");
  const owner = await import("./runtime-qualify.mjs");
  need(owner.isCrateName(unit.name), "runtime crate name invalid");
  const crate = unit.name;
  const readSource = (name) => { need(safe(name), "unsafe source path"); return Buffer.from(source.readData(name)); };
  let prescribed; let trustedWorkloads;
  try { prescribed = owner.readSourcePrescription(crate, readSource); trustedWorkloads = owner.workloadsFor(crate); } catch (error) { need(false, error.message); }
  let budget; let trustedBudget;
  try { budget = JSON.parse(readSource(`crates/${crate}/runtime-budget.json`)); } catch { need(false, "candidate runtime budget missing or not JSON"); }
  try { trustedBudget = JSON.parse(fs.readFileSync(path.join(TRUST_ROOT, `crates/${crate}/runtime-budget.json`), "utf8")); } catch { need(false, "trusted runtime budget missing"); }
  validateFrozenRuntimeBudget(budget, trustedBudget);
  const helpers = trustedRuntimeHelperPaths();
  const trustedHelperHashes = Object.fromEntries(helpers.map((name) => [name, hash(fs.readFileSync(path.join(TRUST_ROOT, name)))]));
  const context = { crate, repository: REPOSITORY, runId: candidate.id, runAttempt: "local", sourceSha: candidate.sourceSha, artifactId: "local", artifactDigest: `local:${manifestSha256}` };
  const qualificationRun = { id: candidate.id, attempt: "local" };
  const newSet = () => ({ records: [], raw: new Map(), profiles: new Map() });
  const addTo = (set, record, raw, profile) => {
    const file = record.evidence.file;
    need(!set.raw.has(file), "duplicate raw record evidence path");
    set.records.push(structuredClone(record)); set.raw.set(file, Buffer.from(raw));
    if (profile) set.profiles.set(file, profile.name);
  };
  const sets = []; const selectedSet = newSet(); let repeatCount;
  for (const host of owner.REQUIRED_HOSTS) {
    const receipt = receipts?.[LOCAL_HOST_KEYS[host]];
    const entry = receipt?.runtime?.crates?.[crate];
    need(receipt?.hostOs === host && typeof receipt.arch === "string" && receipt.runtime?.schema === 1 && receipt.runtime.kind === "local-runtime" && entry && typeof entry === "object", `${host}: no local runtime proof for ${crate} in the pinned receipt`);
    need(entry.kind === "local-runtime-crate" && entry.source === "local" && entry.policy === WINDOW_POLICY.id, `${host}: runtime proof kind, source or window policy mismatch`);
    need(entry.crate === crate && entry.version === unit.version && entry.sourceSha === candidate.sourceSha && entry.candidateId === candidate.id && entry.manifestSha256 === manifestSha256 && entry.archiveSha256 === unit.sha256, `${host}: runtime proof does not bind this crate, version, commit, candidate, manifest and archive`);
    need(entry.host?.os === host && entry.host.arch === receipt.arch && same(entry.broker, receipt.broker), `${host}: runtime proof host, architecture or broker differ from the receipt's`);
    need(entry.harness?.sourceSha === candidate.sourceSha && entry.harness.helpers && same(Object.keys(entry.harness.helpers).sort(), helpers), `${host}: runtime harness inventory differs from the trusted helper closure`);
    for (const [name, digest] of Object.entries(entry.harness.helpers)) need(digest === trustedHelperHashes[name], `${host}: runtime harness differs from trusted supported helper: ${name}`);
    const rows = entry.workloads;
    need(Array.isArray(rows) && rows.length > 0, `${host}: no runtime workload receipts`);
    const bindings = rows[0]?.bindings;
    need(bindings && typeof bindings === "object" && Object.keys(bindings).length > 0, `${host}: runtime source bindings missing`);
    for (const [name, digest] of Object.entries(bindings)) need(hash(readSource(name)) === digest, "source binding differs from original candidate");
    for (const name of [owner.workloadPrescriptionPath(crate), ...owner.trustedRequiredSources(crate), `crates/${crate}/Cargo.toml`, ...prescribed.workloads.map((w) => `crates/${crate}/benches/${w.bench}.rs`)]) need(Object.hasOwn(bindings, name), "required sealed binding absent");
    try { owner.validateWorkloadReceipts(rows, context, unit, manifestSha256, bindings, host, candidate.sourceSha, budget, qualificationRun); } catch (error) { need(false, `${host}: ${error.message}`); }
    for (const row of rows) {
      const workload = trustedWorkloads.find((item) => item.id === row.workload);
      need(workload && row.source === "local", `${host}: unknown or non-local workload receipt`);
      let gate; try { gate = owner.budgetForWorkload(budget, workload); } catch (error) { need(false, error.message); }
      const runs = row.repeats;
      need(Array.isArray(runs) && runs.length >= WINDOW_POLICY.minRepeats && runs.length <= WINDOW_POLICY.maxRepeats && (repeatCount === undefined || runs.length === repeatCount), `${host} ${row.workload}: repeat count is outside the policy or differs between workloads`);
      repeatCount = runs.length;
      let previousEnd = -Infinity;
      runs.forEach((run, index) => {
        const at = `${host} ${row.workload} repeat ${index}`;
        need(run?.index === index && typeof run.raw === "string" && hash(Buffer.from(run.raw)) === run.rawSha256 && run.exitCode === 0 && !run.signal, `${at}: raw output or exit status`);
        need(run.record && run.record.command === row.command && run.record.evidence?.sha256 === run.rawSha256 && run.record.evidence.file === row.record.evidence.file, `${at}: record does not describe its raw output and command`);
        const issues = owner.measurementProblems(run.record, gate, candidate.sourceSha, host);
        need(issues.length === 0, `${at}: ${issues.join("; ")}`);
        const quiet = windowProblems(run.window);
        need(quiet.length === 0, `${at}: window is not quiet: ${quiet.join("; ")}`);
        need(run.window.startedAtMs >= previousEnd, `${at}: window overlaps the previous repeat`);
        previousEnd = run.window.endedAtMs;
        addTo((sets[index] ??= newSet()), run.record, run.raw, workload.profile);
      });
      need(Number.isInteger(row.selected) && row.selected === worstIndex(runs.map((run) => run.record), gate) && row.rawSha256 === runs[row.selected].rawSha256 && same(row.record, runs[row.selected].record), `${host} ${row.workload}: the submitted record is not the worst repeat`);
      addTo(selectedSet, row.record, runs[row.selected].raw, workload.profile);
    }
  }
  // The full owner gate, on both hosts at full scale: once per repeat index, and once for the submitted (worst) records.
  for (const set of [...sets, selectedSet]) validateRuntimeRecords({ source, sourceSha: candidate.sourceSha, unit, budget, records: set.records, rawByEvidence: set.raw, profiles: set.profiles });
  const handle = Object.freeze({ candidateId: candidate.id, proofSha256: localRuntimeProofSha256(receipts, crate) });
  authenticated.set(handle, { candidate: candidateHash(candidate), unit: { name: unit.name, version: unit.version, sha256: unit.sha256 }, reports: selectedSet.records });
  return handle;
}
export function authenticatedRuntimeProof(handle, candidate, unit) {
  const result = authenticated.get(handle);
  need(result && result.candidate === candidateHash(candidate) && ["name", "version", "sha256"].every((key) => result.unit[key] === unit[key]), "unauthenticated runtime evidence");
  return structuredClone(result.reports);
}
