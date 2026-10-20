// Trusted policy over caller-authenticated evidence. Candidate files are DATA, never imported.
import fs from "node:fs";
import path from "node:path";
import crypto from "node:crypto";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { publicationObligationsVerdict } from "./publication-obligations.mjs";
import { TEST_COMMAND } from "./test-results.mjs";
import { featureLabels } from "./publication-record.mjs";
import { cargoRecipe } from "./consumer-recipes.mjs";
import { validatePublicationEvidence } from "../../packages/release/publication-gate.mjs";
import { runtimeBudgetCheck, runtimeBudgetStructureCheck, crateClass, RUNTIME_BUDGET_CRATES } from "../../packages/release/runtime-budgets.mjs";
import { authenticatedRuntimeProof } from "./runtime-supplement.mjs";
import { workloadsFor } from "./runtime-qualify.mjs";
import { readCargoPackage } from "../../packages/release/crate-graph.mjs";
import { npmConsumerQualificationProblems, RECIPE_FILES as NPM_RECIPE_FILES, SOURCE_FILES as NPM_SOURCE_FILES } from "./npm-consumer.mjs";
import { declaredEntries, binSmokePlan } from "./consume.mjs";
import { npmRecipeQualificationProblems, validateRecipeEntry, shortName, sourceFiles as npmRecipeSourceFiles, hasReviewedNpmEntry } from "./npm-recipe-consumer.mjs";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const HOSTS = { macos: "darwin", windows: "win32" };
const HEX = /^[a-f0-9]{64}$/;
const SHA = /^[a-f0-9]{40}$/;
const hash = (bytes) => crypto.createHash("sha256").update(bytes).digest("hex");
const jsonHash = (value) => hash(JSON.stringify(value ?? null));
const same = (a, b) => JSON.stringify(a) === JSON.stringify(b);
const setEqual = (a, b) => Array.isArray(a) && Array.isArray(b) && same([...a].sort(), [...b].sort());
const text = (s) => typeof s === "string" && s.trim().length > 0;
const safe = (s) => typeof s === "string" && s.length > 0 && !/[\\:\x00-\x1f]/.test(s) && s.split("/").every((p) => p && p !== "." && p !== "..");

/** Source structure & authenticated exact-archive workloads are separate conjunctive proofs. */
// `runtimeProof` is one authenticated handle; `runtimeProofs` may carry one per crate of a multi-crate candidate.
// A handle qualifies only the unit it was authenticated for, and the proven workloads come from the trusted
// per-crate coverage table (runtime-qualify.mjs), never from the candidate or the handle.
export function runtimeFeatureProofPolicy({ crate, meta = {}, hasFile = false, runtimeProof = null, runtimeProofs = [], candidate, unit }) {
  const listedOwner = RUNTIME_BUDGET_CRATES.includes(crate);
  const classification = crateClass(meta, { hasFile: hasFile || listedOwner });
  const required = listedOwner || classification.kind === "always-on";
  const result = {
    classification: classification.kind, listedOwner, required,
    status: required ? "HOLD" : "not-applicable",
    failures: required ? ["authenticated isolated runtime feature/workload proof missing"] : [],
  };
  if (required && classification.kind === "always-on") {
    for (const handle of [runtimeProof, ...runtimeProofs].filter(Boolean)) {
      try {
        authenticatedRuntimeProof(handle, candidate, unit);
        const workloads = workloadsFor(unit.name).map((w) => w.id);
        return { ...result, status: "qualified", failures: [], proof: { candidateId: handle.candidateId, proofSha256: handle.proofSha256, workloads } };
      } catch { /* Caller JSON cannot grant runtime qualification; try the next authenticated handle. */ }
    }
  }
  return result;
}
function file(root, relative) {
  if (!safe(relative)) throw new Error("unsafe evidence path");
  const base = fs.realpathSync(root); const full = path.resolve(base, relative);
  for (let cursor = full; cursor !== base; cursor = path.dirname(cursor)) {
    if (fs.lstatSync(cursor).isSymbolicLink()) throw new Error("evidence path contains symlink");
  }
  if (!fs.statSync(full).isFile()) throw new Error("evidence path is not a file");
  return fs.readFileSync(full);
}

/** Validate an actual R2a report against authenticated identity & reviewed recipe DATA. */
export function consumerQualificationProblems({ report, expected, recipe }) {
  const errors = []; const bad = (s) => errors.push(s);
  if (!report || report.schema !== 1 || report.kind !== "registry-consumer" || report.qualification !== "R2a" || report.verified !== true) return ["missing named R2a registry-consumer qualification"];
  for (const key of ["name", "version", "commit", "manifestSha256", "archiveSha256", "host", "runId", "runAttempt", "manifestRunId", "manifestRunAttempt"]) {
    if (String(report[key] ?? "") !== String(expected[key] ?? "") || expected[key] === undefined) bad(`R2a ${key} mismatch`);
  }
  if (!text(report.arch)) bad("R2a native architecture missing");
  if (!recipe || !/^PK\d+$/.test(recipe.pkId ?? "") || recipe.id !== `ci/consumers/${expected.name}/src/main.rs` || !same(report.recipe, recipe)) bad("R2a recipe differs from reviewed source/config hashes");
  if (report.finalSource !== "sealed-directory-registry" || report.finalManifestSha256 !== recipe?.manifestSha256 || !HEX.test(report.lockSha256 ?? "")) bad("R2a final manifest, registry source or lock hash missing");
  if (!Array.isArray(report.resolvedFeatures) || new Set(report.resolvedFeatures).size !== report.resolvedFeatures.length || report.resolvedFeatures.some((feature) => !text(feature)) || !(recipe?.dependency?.features ?? []).every((feature) => report.resolvedFeatures?.includes(feature))) bad("R2a resolved features differ from prescription");
  const selected = Array.isArray(report.lockEvidence) ? report.lockEvidence.filter((row) => row?.name === expected.name) : [];
  if (selected?.length !== 1 || selected[0].version !== expected.version || selected[0].source !== "registry+https://github.com/rust-lang/crates.io-index" || selected[0].checksum !== expected.archiveSha256 || selected[0].archiveSha256 !== expected.archiveSha256) bad("R2a lock does not bind exact sealed archive");
  if (!text(report.toolchain?.rustc) || !text(report.toolchain?.cargo)) bad("R2a native toolchain evidence missing");
  const commands = Array.isArray(report.commands) ? report.commands : [];
  if (!commands.length || commands.some((row) => !row || !text(row.command) || !Array.isArray(row.args) || row.args.some((arg) => typeof arg !== "string") || !text(row.cwd) || row.status !== 0 || typeof row.stdout !== "string" || typeof row.stderr !== "string")) bad("R2a actual command evidence missing or failed");
  for (const stage of ["build", "runtime"]) {
    if (report[stage]?.status !== 0 || report[stage]?.locked !== true || report[stage]?.offline !== true) bad(`R2a ${stage} must succeed locked/offline`);
    const args = [stage === "runtime" ? "run" : "build", "--locked", "--offline"];
    if (!commands.some((row) => row?.command === "cargo" && same(row.args, args) && row.status === 0 && row.cwd === report.consumerDirectory)) bad(`R2a ${stage} actual command missing`);
  }
  const runtime = commands.filter((row) => row?.command === "cargo" && same(row.args, ["run", "--locked", "--offline"]) && row.cwd === report.consumerDirectory);
  if (runtime?.length !== 1 || runtime[0].stdout !== report.runtimeOutput) bad("R2a runtime output differs from actual command");
  try {
    const metadata = commands.filter((row) => row?.command === "cargo" && same(row.args, ["metadata", "--format-version", "1", "--locked", "--offline"]) && row.cwd === report.consumerDirectory);
    if (metadata.length !== 1) throw new Error("missing final metadata command");
    const actual = JSON.parse(metadata[0].stdout);
    const packages = actual.packages.filter((pkg) => pkg.name === expected.name && pkg.version === expected.version);
    const resolution = actual.resolve.nodes.filter((node) => node.id === packages[0]?.id);
    if (packages.length !== 1 || packages[0].source !== "registry+https://github.com/rust-lang/crates.io-index" || resolution.length !== 1 || !same(resolution[0].features, report.resolvedFeatures)) throw new Error("final resolution differs");
  } catch { bad("R2a resolved features lack matching actual final metadata command"); }
  if (expected.name === "rightkit-fs" && (!HEX.test(report.fixture?.stateSha256 ?? "") || !HEX.test(report.fixture?.volumeInfoSha256 ?? "") || report.fixture?.platform !== HOSTS[expected.host])) bad("R2a native filesystem fixture missing");
  return errors;
}

/** No defaults for native run metadata: missing historical records remain missing. */
export function canonicalRunProblems({ evidence, expected, actualResults, host }) {
  const errors = [];
  if (!evidence || evidence.schema !== 1 || !Array.isArray(evidence.runs)) return ["missing canonical publicationEvidence metadata"];
  errors.push(...validatePublicationEvidence({ expected: { ...expected, requiredHosts: [HOSTS[host]] }, evidence }).missing);
  for (const key of ["kind", "name", "version", "commit", "artifactSha256"]) if (evidence[key] !== expected[key]) errors.push(`canonical publicationEvidence ${key} mismatch`);
  if (expected.tree !== undefined && evidence.tree !== expected.tree) errors.push("canonical publicationEvidence tree mismatch");
  const runs = evidence.runs;
  if (runs.length !== 1 || runs[0]?.host?.os !== HOSTS[host]) return [...errors, "canonical publicationEvidence must contain exactly one actual native host run"];
  const tests = (actualResults ?? []).filter((row) => TEST_COMMAND.test(row.command ?? ""));
  if (!tests.length || runs[0].command !== tests.map((row) => row.command).join(" && ") || runs[0].passed !== tests.reduce((sum, row) => sum + row.executed, 0) || runs[0].failed !== tests.reduce((sum, row) => sum + row.failed, 0)) errors.push("canonical native run disagrees with actual command results");
  try {
    const registry = expected.kind === "cargo" ? "crates" : "npm";
    if (!same(runs[0].features, featureLabels(registry, expected.name, tests.map((row) => row.command)))) errors.push("canonical native features differ from actual test commands");
  } catch (error) { errors.push(`canonical native features: ${error.message}`); }
  return errors;
}

function reviewedRecipe(source, unit) {
  // Trust inventory from verifier revision; candidate's inventory cannot grant itself review.
  const config = JSON.parse(file(ROOT, "ci/consumer-recipes.json"));
  const row = config.cargo?.[unit.name] ?? config.crates?.[unit.name];
  if (config.schema !== 1 || !row || !/^PK\d+$/.test(row.pkId ?? "")) throw new Error("missing reviewed named R2a recipe inventory");
  const recipe = cargoRecipe({ root: source.root, unit }).evidence;
  // A reviewed entry must bind every recipe file, rather than merely naming a directory.
  if (row.pkId !== recipe.pkId || row.id !== recipe.id || !row.files || !same(row.files, recipe.files)) throw new Error("candidate PK identity/recipe files differ from reviewed inventory");
  return { ...recipe, sourceCommit: source.source.sha };
}

/** Match unchanged generic portability to authenticated manifest declarations, without waivers. */
export function npmPortabilityQualificationProblems({ packageManifest, portability } = {}) {
  const declared = declaredEntries(packageManifest);
  const runtime = declared.subpaths.filter(entry => entry.runtime.length > 0);
  // consume.mjs CHECK_SCRIPT imports JavaScript & resolves other declared assets only.
  // Any JavaScript alternative still requires import evidence; mixed branches fail closed.
  const isJavaScript = file => /\.[cm]?js$/.test(new URL(file, "file:///rightkit-consumer/").pathname);
  const imported = runtime.filter(entry => entry.runtime.some(isJavaScript)).map(entry => entry.spec);
  const resolvedOnly = runtime.filter(entry => entry.runtime.every(file => !isJavaScript(file))).map(entry => entry.spec);
  const smokePlan = binSmokePlan(packageManifest, declared);
  const smoke = portability?.binSmoke;
  const completeSmoke = !smokePlan.problems.length && Array.isArray(smoke) && smoke.length === smokePlan.checks.length && new Set(smoke.map(row => row?.bin)).size === smoke.length && smoke.every(row => {
    const check = smokePlan.checks.find(check => check.bin === row?.bin);
    return check && /\.[cm]?js$/.test(check.file) && row && !Array.isArray(row) && setEqual(Object.keys(row), ["bin", "args", "status"]) && same(row.args, check.args) && row.status === check.exitCode;
  });
  if (!portability || portability.entries !== runtime.length || portability.filesChecked !== declared.files.length || portability.noJsEntry !== (runtime.length === 0) || !setEqual(portability.imported, imported) || !setEqual(portability.resolvedOnly, resolvedOnly) || !setEqual(portability.patterns, declared.patterns) || !setEqual(portability.bins, declared.bins) || ["productionRejected", "conditionalImports", "skippedImport"].some(field => !setEqual(portability[field], [])) || !completeSmoke || runtime.some(entry => entry.testConditions != null) || declared.patterns.length) return ["named npm R2a lacks complete unchanged generic portability proof"];
  return [];
}

/** Named npm seam: authenticated candidate DATA & trusted verifier recipe inventory only. */
export function namedNpmQualificationProblems({ source, candidate, manifest, unit, host, report, publicationEvidence } = {}) {
  const problems = [];
  const logs = unit?.name === "@rightkit/logs";
  // Trust inventory from verifier revision; a candidate cannot grant itself a reviewed npm entry.
  if (!logs && !hasReviewedNpmEntry(JSON.parse(file(ROOT, "ci/consumer-recipes.json")), unit?.name)) return ["missing reviewed named npm R2a recipe; generic import is not PK qualification"];
  try {
    if (typeof source?.readData !== "function" || source.source?.sha !== candidate?.sourceSha || manifest?.commit !== candidate?.sourceSha || candidate.registry !== "npm") throw new Error("authenticated npm source reader/identity missing");
    const config = JSON.parse(file(ROOT, "ci/consumer-recipes.json"));
    const recipe = config.schema === 1 ? config.npm?.[unit.name] : undefined;
    if (!recipe) throw new Error("missing trusted reviewed npm recipe inventory");
    const sourceBindings = {}, data = {};
    const dir = logs ? "packages/logs" : `packages/${shortName(unit.name)}`;
    const bound = logs ? [...NPM_RECIPE_FILES, ...NPM_SOURCE_FILES] : [...Object.keys(validateRecipeEntry(recipe, unit.name).files), ...npmRecipeSourceFiles(unit.name)];
    for (const relative of bound) {
      const raw = source.readData(relative);
      if (!Buffer.isBuffer(raw)) throw new Error(`authenticated source did not return exact blob bytes: ${relative}`);
      data[relative] = raw;
      sourceBindings[relative] = { sha256: hash(raw), gitBlob: crypto.createHash("sha1").update(Buffer.concat([Buffer.from(`blob ${raw.length}\0`), raw])).digest("hex") };
    }
    const rootManifest = JSON.parse(data["package.json"]);
    const packageManifest = JSON.parse(data[`${dir}/package.json`]);
    if (unit.dir !== dir || packageManifest.name !== unit.name || packageManifest.version !== unit.version || rootManifest.packageManager !== packageManifest.packageManager) throw new Error("authenticated npm manifest/version/pnpm pin mismatch");
    const native = publicationEvidence?.runs;
    if (!Array.isArray(native) || native.length !== 1 || native[0]?.host?.os !== HOSTS[host] || !text(native[0]?.host?.arch)) throw new Error("actual native publication run missing");
    const expected = { name: unit.name, version: unit.version, commit: candidate.sourceSha, runId: String(candidate.runId), runAttempt: String(candidate.runAttempt), manifestRunId: String(manifest.runId), manifestRunAttempt: String(manifest.runAttempt), manifestSha256: candidate.manifestSha256, archiveSha256: unit.sha256, archiveSize: unit.size, archiveIntegrity: unit.integrity, host, runnerOS: host === "windows" ? "Windows" : "macOS", arch: native[0].host.arch, packageManager: rootManifest.packageManager };
    problems.push(...(logs ? npmConsumerQualificationProblems : npmRecipeQualificationProblems)({ report, expected, recipe, sourceBindings }));
    // Preserve generic portability's complete installed-entry proof, even alongside named runtime.
    problems.push(...npmPortabilityQualificationProblems({ packageManifest, portability: report?.portability }));
  } catch (error) { problems.push(`named npm R2a: ${error.message}`); }
  return problems;
}

/** Caller authenticates provenance first; this function validates contents & never publishes. */
export function semanticAdmission({ candidate, manifest, source, receipts = {}, qa = {}, packDir, env = {}, spawn = spawnSync, runtimeProof = null, runtimeProofs = [] } = {}) {
  const failures = []; const evidenceHashes = {}; const hostVerdicts = {}; const archives = {};
  const fail = (s) => failures.push(s);
  const attempt = (fn, label) => { try { return fn(); } catch (error) { fail(`${label}: ${error.message}`); return null; } };
  if (!candidate || !manifest || !source || !SHA.test(source?.source?.sha ?? "") || !SHA.test(source?.source?.tree ?? "") || source.source.sha !== candidate.sourceSha || manifest.commit !== candidate.sourceSha || manifest.registry !== candidate.registry || !["crates", "npm"].includes(candidate.registry)) fail("candidate/source/manifest identity mismatch");
  const units = Array.isArray(manifest?.units) ? manifest.units : [];
  if (!units.length || !setEqual(units.map((u) => u.name), candidate?.units) || new Set(units.map((u) => u.name)).size !== units.length) fail("manifest unit coverage differs from candidate");
  const rawManifest = attempt(() => file(packDir, "manifest.json"), "raw manifest");
  if (rawManifest) {
    evidenceHashes.manifest = hash(rawManifest);
    if (evidenceHashes.manifest !== candidate?.manifestSha256 || !same(attempt(() => JSON.parse(rawManifest), "manifest JSON"), manifest)) fail("raw manifest differs from authenticated manifest");
  }
  for (const host of Object.keys(HOSTS)) {
    const obligation = source?.hosts?.[host];
    if (obligation?.registry !== candidate?.registry || !setEqual(obligation?.units, candidate?.units) || obligation?.host !== host) fail(`${host}: source obligations identity mismatch`);
    hostVerdicts[host] = attempt(() => publicationObligationsVerdict(obligation, qa[host], candidate?.sourceSha), `${host} obligations`) ?? { ok: false, failures: ["cannot validate source obligations"] };
    evidenceHashes[host] = { receipt: jsonHash(receipts[host]), summary: jsonHash(qa[host]?.summary), unitEvidence: jsonHash(qa[host]?.unitEvidence), obligations: jsonHash(obligation) };
    const rawHashes = { summary: qa[host]?.summarySha256, unitEvidence: qa[host]?.unitEvidenceSha256 };
    evidenceHashes[host].raw = rawHashes;
    const receipt = receipts[host];
    for (const [field, key] of [["allowHostExclusion", "hostExclusion"], ["allowNoTest", "noTest"], ["allowIgnored", "ignored"], ["allowMissingAssets", "missingAssets"]]) {
      if (!setEqual(receipt?.[field], candidate?.allow?.[key] ?? [])) fail(`${host}: ${field} differs from authenticated request`);
    }
    if (!HEX.test(rawHashes.summary ?? "") || !HEX.test(rawHashes.unitEvidence ?? "")) fail(`${host}: authenticated raw QA evidence hashes missing`);
    if (receipt?.sourceGate?.summarySha256 !== rawHashes.summary || receipt?.sourceGate?.unitEvidenceSha256 !== rawHashes.unitEvidence) fail(`${host}: sourceGate differs from original raw QA evidence`);
    if (receipt?.sourceGate?.tree !== source?.source?.tree || !same(receipt?.sourceGate?.dependentClosure ?? null, qa[host]?.unitEvidence?.dependentClosure ?? null)) fail(`${host}: sourceGate tree/dependent closure mismatch`);
    if (receipt?.schema !== 1 || receipt.kind !== "verify" || receipt.host !== host || receipt.registry !== candidate?.registry || receipt.commit !== candidate?.sourceSha || receipt.manifestSha256 !== candidate?.manifestSha256 || String(receipt.runId) !== String(candidate?.runId) || String(receipt.runAttempt) !== String(candidate?.runAttempt) || String(receipt.manifestRunId) !== String(manifest?.runId) || String(receipt.manifestRunAttempt) !== String(manifest?.runAttempt) || !setEqual(receipt.units?.map((u) => u.name), candidate?.units) || new Set(receipt.units?.map((u) => u.name)).size !== units.length) fail(`${host}: receipt identity/coverage mismatch`);
  }
  const unitVerdicts = units.map((unit) => {
    const problems = [...failures]; const adoptionPending = []; const policies = {};
    const bad = (s) => problems.push(s);
    let bytes; let packageInfo; let unitRoot;
    try {
      bytes = file(packDir, unit.file); archives[unit.name] = hash(bytes);
      if (archives[unit.name] !== unit.sha256 || bytes.length !== unit.size) bad("actual archive bytes/hash/size differ from manifest");
      if (!safe(unit.dir) || !unit.dir.startsWith(candidate.registry === "crates" ? "crates/" : "packages/") || unit.dir.split("/").length !== 2) throw new Error("unsafe unit source directory");
      unitRoot = path.join(source.root, unit.dir);
      packageInfo = candidate.registry === "crates" ? readCargoPackage(unitRoot) : JSON.parse(file(source.root, `${unit.dir}/package.json`));
      if (packageInfo.name !== unit.name || packageInfo.version !== unit.version) bad("unit name/version differs from authenticated source manifest");
    } catch (error) { bad(`source/archive: ${error.message}`); }
    const records = [];
    for (const host of Object.keys(HOSTS)) {
      const plan = source?.hosts?.[host]; const policy = plan?.unitPolicies?.find((p) => p.name === unit.name); policies[host] = policy?.status ?? "required-positive-tests";
      for (const error of hostVerdicts[host].failures) bad(`${host}: ${error}`);
      const row = receipts[host]?.units?.find((r) => r.name === unit.name);
      if (!row || row.file !== unit.file || row.sha256 !== unit.sha256 || row.version !== unit.version || row.bytesVerified !== true) bad(`${host}: receipt does not bind exact archive`);
      if (policy?.status) bad(`${host}: ${policy.status} is explicit source policy; canonical publication gate requires positive native tests (policy conflict)`);
      const actualResults = qa[host]?.summary?.results?.filter((r) => r.unit === policy?.id) ?? [];
      const expected = { kind: candidate.registry === "crates" ? "cargo" : "npm", name: unit.name, version: unit.version, commit: candidate.sourceSha, tree: source?.source?.tree, artifactSha256: unit.sha256 };
      for (const error of canonicalRunProblems({ evidence: row?.publicationEvidence, expected, actualResults, host })) bad(`${host}: ${error}`);
      if (row?.publicationEvidence) records.push(row.publicationEvidence);
      const report = row?.consumer;
      if (report?.arch !== row?.publicationEvidence?.runs?.[0]?.host?.arch) bad(`${host}: R2a native architecture differs from actual host run`);
      if (candidate.registry === "npm") {
        for (const error of namedNpmQualificationProblems({ source, candidate, manifest, unit, host, report, publicationEvidence: row?.publicationEvidence })) bad(`${host}: ${error}`);
      }
      else {
        let recipe;
        try { recipe = reviewedRecipe(source, unit); } catch (error) { bad(`${host}: ${error.message}`); }
        const expectedConsumer = { name: unit.name, version: unit.version, commit: candidate.sourceSha, manifestSha256: candidate.manifestSha256, archiveSha256: unit.sha256, host, runId: candidate.runId, runAttempt: candidate.runAttempt, manifestRunId: manifest.runId, manifestRunAttempt: manifest.runAttempt };
        for (const error of consumerQualificationProblems({ report, expected: expectedConsumer, recipe })) bad(`${host}: ${error}`);
      }
    }
    // Combining real runs changes no run fields & supplies no historical metadata.
    const combined = records.length === 2 ? { ...records[0], runs: records.flatMap((record) => record.runs ?? []) } : undefined;
    const canonical = validatePublicationEvidence({ expected: { kind: candidate?.registry === "crates" ? "cargo" : "npm", name: unit.name, version: unit.version, commit: candidate?.sourceSha, tree: source?.source?.tree }, evidence: combined, artifactBytes: bytes });
    canonical.missing.forEach((error) => bad(`publication gate: ${error}`));
    let runtime = null;
    let runtimeFeatureProof = null;
    const runtimeInputHashes = {};
    if (candidate?.registry === "crates" && unitRoot) {
      try {
        const meta = packageInfo?.manifest?.package?.metadata?.rightkit ?? {};
        const budgetFile = `${unit.dir}/runtime-budget.json`;
        const hasBudgetFile = fs.existsSync(path.join(source.root, budgetFile));
        runtimeFeatureProof = runtimeFeatureProofPolicy({ crate: unit.name, meta, hasFile: hasBudgetFile, runtimeProof, runtimeProofs, candidate, unit });
        runtimeFeatureProof.failures.forEach(bad);
        const gitEnv = Object.fromEntries(Object.entries(env).filter(([key]) => !/^GIT_/i.test(key)));
        Object.assign(gitEnv, { GIT_CONFIG_NOSYSTEM: "1", GIT_CONFIG_GLOBAL: process.platform === "win32" ? "NUL" : "/dev/null", GIT_NO_REPLACE_OBJECTS: "1", GIT_TERMINAL_PROMPT: "0", GIT_OPTIONAL_LOCKS: "0" });
        const git = (args, cwd) => {
          const result = spawn("git", ["-c", "core.hooksPath=", "-c", "core.fsmonitor=false", ...args], { cwd, env: gitEnv, encoding: "utf8", windowsHide: true, shell: false, timeout: 30_000, maxBuffer: 32 * 1024 * 1024 });
          return { status: result.error || result.signal ? 1 : result.status ?? 1, stdout: String(result.stdout ?? "").trim() };
        };
        const runtimeCheck = runtimeFeatureProof.status === "qualified" ? runtimeBudgetStructureCheck : runtimeBudgetCheck;
        runtime = runtimeCheck({ crate: unit.name, crateDir: unitRoot, version: unit.version, evidence: combined }, { git });
        if (runtime.status !== "ok") bad(`runtime budget: ${runtime.detail}`);
        if (hasBudgetFile) {
          const raw = file(source.root, budgetFile); runtimeInputHashes[budgetFile] = hash(raw);
          for (const measurement of JSON.parse(raw).measurements ?? []) {
            if (typeof measurement?.evidence?.file === "string") {
              const relative = `${unit.dir}/${measurement.evidence.file}`;
              runtimeInputHashes[relative] = hash(file(source.root, relative));
            }
          }
        }
        if (meta?.continuous === "held-resource") adoptionPending.push("held-resource structural publication check only; installed-app runtime measurement remains pending");
      } catch (error) { bad(`runtime budget: ${error.message}`); }
    }
    return { name: unit.name, version: unit.version, archiveSha256: archives[unit.name] ?? null, ok: problems.length === 0, policies, failures: problems, adoptionPending, runtime, runtimeFeatureProof, runtimeInputHashes, evidenceSha256: jsonHash({ receipts: Object.fromEntries(Object.keys(HOSTS).map((h) => [h, receipts[h]?.units?.filter((r) => r.name === unit.name)])), runtime, runtimeFeatureProof, runtimeInputHashes }) };
  });
  return { schema: 1, kind: "semantic-admission", ok: failures.length === 0 && unitVerdicts.every((u) => u.ok), source: source?.source ?? null, manifestSha256: evidenceHashes.manifest ?? null, failures, units: unitVerdicts, hostVerdicts, evidenceHashes, sourceHashes: source?.sourceHashes ?? {}, archiveHashes: archives };
}
