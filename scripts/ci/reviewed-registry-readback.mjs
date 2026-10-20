// Promotion-aware R2b. Execute only trusted checkout code; original artifacts remain data.
import fs from "node:fs";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { REPOSITORY, APPROVED_ARTIFACTS_FILE, sha, integrity, loadSealed } from "./upload.mjs";
import { selectCandidate, prepareCandidate, unpackArtifact } from "./reviewed-candidate.mjs";
import { assertRegistryManifest, registryLockEvidence } from "./registry-consumer.mjs";
import { cargoRecipe, planRuns, executeRuns, planNativeHelper, stageNativeHelper, nativeHelperExecute } from "./consumer-recipes.mjs";
import { lockEvidence, registryUrl } from "./registry-readback.mjs";

const need = (ok, why) => { if (!ok) throw new Error(`reviewed registry readback refused: ${why}`); };
const same = (a, b) => JSON.stringify(a) === JSON.stringify(b);
const numeric = (v) => /^[1-9][0-9]*$/.test(String(v ?? "")) && Number.isSafeInteger(Number(v));
const hash = (bytes) => sha("sha256", bytes);
const read = (file) => JSON.parse(fs.readFileSync(file, "utf8"));
const write = (file, value) => fs.writeFileSync(file, `${JSON.stringify(value, null, 2)}\n`);
const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const WORKFLOW = ".github/workflows/publish-reviewed.yml";
const READBACK = ".github/workflows/reviewed-registry-readback.yml";
const within = (root, file) => { const rel = path.relative(root, file); return rel && !path.isAbsolute(rel) && rel !== ".." && !rel.startsWith(`..${path.sep}`); };

export function authenticatePromotion({ candidate, repository, workflow, run, jobs, comparison, runId }) {
  need(numeric(runId) && numeric(run?.run_attempt), "explicit numeric promotion run/attempt required");
  need(numeric(repository?.id) && repository.full_name === REPOSITORY && repository.default_branch === "main" && repository.private === false, "wrong public repository");
  need(numeric(workflow?.id) && workflow.path === WORKFLOW && workflow.state === "active", "wrong promotion workflow");
  need(String(run.id) === String(runId) && run.workflow_id === workflow.id && run.path === WORKFLOW, "promotion workflow/run mismatch");
  for (const source of [run.repository, run.head_repository]) need(source?.id === repository.id && source.full_name === REPOSITORY, "foreign promotion repository");
  need(run.event === "workflow_dispatch" && run.head_branch === "main" && /^[0-9a-f]{40}$/.test(run.head_sha ?? "") && run.status === "completed" && run.conclusion === "success", "promotion event/ref/status mismatch");
  need(run.head_sha !== candidate.sourceSha && comparison.status === "ahead" && comparison.merge_base_commit?.sha === candidate.sourceSha, "promotion must descend from sealed source");
  need(Array.isArray(jobs?.jobs) && jobs.total_count === jobs.jobs.length && new Set(jobs.jobs.map(j => j.id)).size === jobs.jobs.length, "incomplete/duplicate promotion jobs");
  need(jobs.jobs.every(j => numeric(j.id) && String(j.run_id) === String(runId) && Number(j.run_attempt) === Number(run.run_attempt) && j.head_sha === run.head_sha), "promotion job run/attempt/source mismatch");
  for (const name of ["seal-check", "upload"]) {
    const rows = jobs.jobs.filter(j => j.name === name);
    need(rows.length === 1 && rows[0].status === "completed" && rows[0].conclusion === "success", `successful ${name} job required exactly once`);
  }
  const upload = jobs.jobs.find(j => j.name === "upload");
  const step = candidate.registry === "npm" ? "Reauthenticate candidate & publish original npm archives" : "Reauthenticate candidate & publish original Cargo archives";
  need(upload.steps?.filter(s => s.name === step && s.status === "completed" && s.conclusion === "success").length === 1, "successful exact registry publication step required");
  need(upload.steps?.filter(s => s.name === "Keep publication receipt" && s.conclusion === "success").length === 1, "successful publication receipt upload required");
  return upload;
}

export function publicationArtifact(artifact, repository, run, upload) {
  need(numeric(artifact?.id) && artifact.name === "publication-receipt" && artifact.expired === false && /^sha256:[0-9a-f]{64}$/.test(artifact.digest ?? ""), "invalid publication artifact");
  const source = artifact.workflow_run;
  need(String(source?.id) === String(run.id) && source.head_sha === run.head_sha && source.head_branch === "main" && source.repository_id === repository.id && source.head_repository_id === repository.id, "foreign publication artifact");
  const created = Date.parse(artifact.created_at); const start = Date.parse(upload.started_at); const end = Date.parse(upload.completed_at);
  need(Number.isFinite(created) && created >= start && created <= end, "publication artifact is outside successful upload attempt");
  return artifact;
}

export function validatePublication(receipt, manifest, candidate, run) {
  need(receipt.schema === 1 && receipt.repository === REPOSITORY && receipt.registry === candidate.registry && receipt.dryRun === false && receipt.commit === candidate.sourceSha && receipt.promotionCommit === run.head_sha && String(receipt.runId) === String(run.id) && String(receipt.runAttempt) === String(run.run_attempt), "publication receipt identity mismatch");
  need(same(receipt.candidate, candidate), "publication receipt candidate/manifest pin mismatch");
  need(manifest.registry === candidate.registry && manifest.commit === candidate.sourceSha && String(manifest.runId) === String(candidate.runId) && String(manifest.runAttempt) === String(candidate.runAttempt), "sealed manifest identity mismatch");
  need(Array.isArray(manifest.units) && same(manifest.units.map(u => u.name), candidate.units), "sealed unit set mismatch");
  need(Array.isArray(receipt.receipts) && receipt.receipts.length === manifest.units.length && new Set(receipt.receipts.map(r => r.unit)).size === manifest.units.length, "publication receipt unit set mismatch");
  for (const unit of manifest.units) {
    need(/^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:[-+][A-Za-z0-9.-]+)?$/.test(unit.version ?? "") && /^[0-9a-f]{64}$/.test(unit.sha256 ?? ""), "invalid sealed identity");
    const row = receipt.receipts.find(r => r.unit === unit.name);
    need(row && row.version === unit.version && row.artifact === unit.file && row.sha256 === unit.sha256 && row.published === true && row.registryVerified === true && row.dryRun === false && row.registry === candidate.registry && row.commit === candidate.sourceSha && String(row.attempt) === String(run.run_attempt) && ["published", "published-by-earlier-attempt"].includes(row.outcome), `unverified publication ${unit.name}`);
    if (candidate.registry === "npm") need(row.sha1 === unit.sha1, "npm receipt hash mismatch");
  }
  return manifest.units;
}

export function recipeDirectory(root, unit, registry) {
  need((registry === "crates" ? /^rightkit-[a-z0-9][a-z0-9-]*$/ : /^@rightkit\/[a-z0-9][a-z0-9-]*$/).test(unit.name), "unsafe recipe unit");
  const base = path.resolve(root, "ci/consumers"); const recipe = path.resolve(base, unit.name);
  need(within(base, recipe) && fs.existsSync(recipe) && fs.lstatSync(recipe).isDirectory() && !fs.lstatSync(recipe).isSymbolicLink(), `missing committed consumer recipe: ${unit.name}`);
  need(within(fs.realpathSync(base), fs.realpathSync(recipe)), "consumer recipe escapes committed recipe root");
  // Reject links, hidden configuration, generated outputs & any dependency redirection.
  const visit = dir => { for (const ent of fs.readdirSync(dir, { withFileTypes: true })) { need(!ent.isSymbolicLink() && !ent.name.startsWith(".") && !["node_modules", "target", "Cargo.lock", "package-lock.json"].includes(ent.name), "unsafe consumer recipe entry"); const file = path.join(dir, ent.name); if (ent.isDirectory()) visit(file); else need(ent.isFile(), "special recipe file"); } };
  visit(recipe);
  if (registry === "crates") {
    const text = fs.readFileSync(path.join(recipe, "Cargo.toml"), "utf8"); assertRegistryManifest(text, unit);
    need(!/\b(?:registry|registry-index)\s*=/.test(text) && !/^\s*\[(?:target|build-dependencies|dev-dependencies)/m.test(text), "unsupported consumer registry/dependency override");
    need(fs.statSync(path.join(recipe, "src/main.rs")).isFile(), "missing Cargo public API example");
  } else {
    const pkg = read(path.join(recipe, "package.json"));
    need(pkg.private === true && pkg.type === "module" && pkg.dependencies?.[unit.name] === unit.version && !pkg.overrides && !pkg.workspaces && !pkg.scripts && !pkg.devDependencies && !pkg.optionalDependencies, "npm recipe requires exact dependencies & no overrides/scripts");
    need(Object.values(pkg.dependencies).every(v => typeof v === "string" && /^\d+\.\d+\.\d+(?:[-+][A-Za-z0-9.-]+)?$/.test(v)), "npm recipe dependencies must be exact public versions");
    need(fs.statSync(path.join(recipe, "consumer.mjs")).isFile(), "missing npm public API example");
  }
  return recipe;
}

async function github(route, env, fetchImpl, binary = false) {
  let response = await fetchImpl(`https://api.github.com/repos/${REPOSITORY}${route ? `/${route}` : ""}`, { headers: { authorization: `Bearer ${env.GITHUB_TOKEN}`, accept: "application/vnd.github+json", "x-github-api-version": "2022-11-28" }, redirect: "manual", signal: AbortSignal.timeout(60_000) });
  if (binary && response.status === 302) {
    const url = new URL(response.headers.get("location"));
    need(url.protocol === "https:" && !url.username && !url.password && !url.port && (url.hostname.endsWith(".blob.core.windows.net") || url.hostname.endsWith(".actions.githubusercontent.com")), "unsafe artifact redirect");
    response = await fetchImpl(url, { redirect: "error", signal: AbortSignal.timeout(120_000) });
  }
  need(response.ok, `GitHub evidence HTTP ${response.status}`);
  return binary ? Buffer.from(await response.arrayBuffer()) : response.json();
}

function fileHashes(root) {
  const hashes = {};
  function visit(dir) { for (const ent of fs.readdirSync(dir, { withFileTypes: true })) { const full = path.join(dir, ent.name); need(!ent.isSymbolicLink(), "bundle symlink"); if (ent.isDirectory()) visit(full); else { need(ent.isFile(), "bundle special file"); const rel = path.relative(root, full).replaceAll("\\", "/"); if (rel !== "transfer.json") hashes[rel] = hash(fs.readFileSync(full)); } } }
  visit(root); return Object.fromEntries(Object.entries(hashes).sort(([a], [b]) => a.localeCompare(b)));
}

export function verifyBundle(bundle, expectedHash, identity) {
  need(/^[0-9a-f]{64}$/.test(expectedHash ?? ""), "trusted transfer digest required");
  const bytes = fs.readFileSync(path.join(bundle, "transfer.json")); need(hash(bytes) === expectedHash, "bundle index digest mismatch");
  const index = JSON.parse(bytes); need(same(index.files, fileHashes(bundle)), "bundle file set/hash mismatch");
  need(index.runId === identity.GITHUB_RUN_ID && index.runAttempt === identity.GITHUB_RUN_ATTEMPT && index.commit === identity.GITHUB_SHA, "foreign readback transfer run/attempt/commit");
  return index;
}

export async function prepareReadback({ env, candidateId, runId, bundle, approvedFile = APPROVED_ARTIFACTS_FILE, root = ROOT, fetchImpl = fetch, spawn = spawnSync }) {
  need(env.GITHUB_ACTIONS === "true" && env.GITHUB_REPOSITORY === REPOSITORY && env.GITHUB_REF === "refs/heads/main" && env.GITHUB_WORKFLOW_REF === `${REPOSITORY}/${READBACK}@refs/heads/main` && /^[0-9a-f]{40}$/.test(env.GITHUB_SHA ?? "") && env.GITHUB_TOKEN && env.RUNNER_TEMP, "trusted readback main Actions context required");
  need(numeric(runId), "explicit numeric promotion run ID required; paths/URLs forbidden");
  need(!fs.existsSync(bundle) && within(path.resolve(env.RUNNER_TEMP), path.resolve(bundle)), "fresh bundle must be under runner temp");
  const candidate = selectCandidate(approvedFile, candidateId);
  const api = (route, binary = false) => github(route, env, fetchImpl, binary);
  const repository = await api(""); const workflow = await api("actions/workflows/publish-reviewed.yml"); const latest = await api(`actions/runs/${runId}`);
  need(numeric(latest.run_attempt), "invalid promotion attempt");
  const attempt = `actions/runs/${runId}/attempts/${latest.run_attempt}`;
  const run = await api(attempt); const jobs = await api(`${attempt}/jobs?per_page=100`); const comparison = await api(`compare/${candidate.sourceSha}...${run.head_sha}`);
  const upload = authenticatePromotion({ candidate, repository, workflow, run, jobs, comparison, runId });
  const ancestry = await api(`compare/${run.head_sha}...${env.GITHUB_SHA}`);
  need(["ahead", "identical"].includes(ancestry.status) && ancestry.merge_base_commit?.sha === run.head_sha, "readback code must descend from promotion commit");
  // Reuse original pinned archive/native proof admission. Context is reconstructed only
  // after authenticating actual trusted promotion run; no original-run source is executed.
  const original = await prepareCandidate({ env: { ...env, GITHUB_SHA: run.head_sha, GITHUB_WORKFLOW_REF: `${REPOSITORY}/${WORKFLOW}@refs/heads/main` }, candidateId, approvedFile, fetchImpl, spawn });
  const listed = await api(`actions/runs/${runId}/artifacts?per_page=100`);
  need(Array.isArray(listed.artifacts) && listed.total_count === listed.artifacts.length, "incomplete publication artifact listing");
  const rows = listed.artifacts.filter(a => a.name === "publication-receipt"); need(rows.length === 1, "exactly one publication artifact required");
  const artifact = publicationArtifact(await api(`actions/artifacts/${rows[0].id}`), repository, run, upload);
  need(artifact.id === rows[0].id && artifact.digest === rows[0].digest, "publication artifact changed");
  const bytes = await api(`actions/artifacts/${artifact.id}/zip`, true); need(`sha256:${hash(bytes)}` === artifact.digest, "publication ZIP digest mismatch");
  const receiptDir = path.join(env.RUNNER_TEMP, `readback-publication-${runId}-${run.run_attempt}`);
  const entries = unpackArtifact({ bytes, zipFile: `${receiptDir}.zip`, destination: receiptDir, spawn });
  need(same(entries, ["publication-receipt.json"]), "publication artifact must contain exactly one receipt");
  const receipt = read(path.join(receiptDir, entries[0])); const manifest = read(path.join(original.packDir, "manifest.json"));
  const units = validatePublication(receipt, manifest, candidate, run);
  for (const unit of units) recipeDirectory(root, unit, candidate.registry);
  const current = await api(`actions/runs/${runId}`);
  need(current.run_attempt === run.run_attempt && current.head_sha === run.head_sha && current.status === "completed" && current.conclusion === "success", "promotion changed during evidence collection");
  fs.mkdirSync(bundle); fs.cpSync(original.packDir, path.join(bundle, "packed"), { recursive: true });
  write(path.join(bundle, "evidence.json"), { candidate, run, artifact, publicationReceipt: receipt, receiptSha256: hash(fs.readFileSync(path.join(receiptDir, entries[0]))) });
  write(path.join(bundle, "transfer.json"), { schema: 1, runId: env.GITHUB_RUN_ID, runAttempt: env.GITHUB_RUN_ATTEMPT, commit: env.GITHUB_SHA, files: fileHashes(bundle) });
  const digest = hash(fs.readFileSync(path.join(bundle, "transfer.json")));
  if (env.GITHUB_OUTPUT) fs.appendFileSync(env.GITHUB_OUTPUT, `transfer_sha256=${digest}\n`);
  return digest;
}

async function publicGet(url, fetchImpl, binary = false) {
  const response = await fetchImpl(url, { redirect: "error", headers: { accept: binary ? "application/octet-stream" : "application/json", "user-agent": "rightkit-reviewed-registry-readback" }, signal: AbortSignal.timeout(60_000) });
  need(response.ok, `public registry HTTP ${response.status}`);
  const bytes = Buffer.from(await response.arrayBuffer()); need(bytes.length <= 64 * 1024 * 1024, "registry response exceeds bound");
  return binary ? bytes : JSON.parse(bytes);
}

export async function registryArchive(unit, registry, fetchImpl = fetch) {
  if (registry === "npm") {
    const metadata = await publicGet(`https://registry.npmjs.org/${encodeURIComponent(unit.name)}/${unit.version}`, fetchImpl);
    need(metadata.name === unit.name && metadata.version === unit.version && metadata.dist?.integrity === unit.integrity && metadata.dist?.shasum === unit.sha1, "npm metadata/sealed checksum mismatch");
    const bytes = await publicGet(registryUrl(metadata.dist.tarball), fetchImpl, true);
    need(hash(bytes) === unit.sha256 && sha("sha1", bytes) === unit.sha1 && integrity(bytes) === unit.integrity, "npm registry archive differs from sealed bytes");
    return { metadata, sha256: hash(bytes) };
  }
  need(registry === "crates" && /^rightkit-[a-z0-9][a-z0-9-]*$/.test(unit.name), "unsupported public registry identity");
  const n = unit.name; const index = `https://index.crates.io/${n.slice(0, 2)}/${n.slice(2, 4)}/${n}`;
  const response = await fetchImpl(index, { redirect: "error", headers: { "user-agent": "rightkit-reviewed-registry-readback" }, signal: AbortSignal.timeout(60_000) });
  need(response.ok, "crates index unavailable"); const text = await response.text(); need(text.length <= 8 * 1024 * 1024, "crates index exceeds bound");
  const rows = text.split(/\r?\n/).filter(Boolean).map(line => JSON.parse(line)).filter(row => row.vers === unit.version);
  need(rows.length === 1 && rows[0].name === n && rows[0].cksum === unit.sha256 && rows[0].yanked === false, "crates index/sealed checksum mismatch");
  const bytes = await publicGet(`https://static.crates.io/crates/${n}/${n}-${unit.version}.crate`, fetchImpl, true);
  need(hash(bytes) === unit.sha256, "registry crate archive differs from sealed bytes");
  return { metadata: rows[0], sha256: hash(bytes) };
}

export function cargoLockEvidence(lock, units, selected) {
  const blocks = lock.split(/^\[\[package\]\]\s*$/m).slice(1);
  for (const block of blocks) {
    const source = block.match(/^source = "([^"]+)"$/m)?.[1];
    // Root consumer alone has no source. Every resolved dependency must be crates.io.
    if (!source) need(/^name = "rightkit-[a-z0-9-]+(?:registry|native)-consumer"$/m.test(block), "unregistered/path Cargo dependency");
    else need(source === "registry+https://github.com/rust-lang/crates.io-index" && /^checksum = "[0-9a-f]{64}"$/m.test(block), "foreign Cargo source/checksum");
  }
  const rows = registryLockEvidence(lock, units);
  need(rows.filter(row => row.name === selected.name).length === 1, "selected exact registry crate absent from lock");
  return rows;
}

/** Child env for one readback consumer: runner basics only, never a token. R2b matches R2a's GITHUB_ACTIONS/RUNNER_TEMP for crates (neither is a credential). */
export function readbackChildEnv(env, scratch, registry) {
  const child = Object.fromEntries(Object.entries(env).filter(([key]) => /^(?:PATH|PATHEXT|SYSTEMROOT|WINDIR|COMSPEC|TEMP|TMP|TMPDIR|USERPROFILE|APPDATA|LOCALAPPDATA|SYSTEMDRIVE|PROCESSOR_ARCHITECTURE|LANG|LC_ALL)$/i.test(key)));
  // Fresh home, config, cache & targets; neither checkout nor runner Cargo/npm state participates.
  Object.assign(child, { HOME: path.join(scratch, "home"), RUSTUP_HOME: env.RUSTUP_HOME || path.join(env.HOME || env.USERPROFILE, ".rustup"), CARGO_HOME: path.join(scratch, "cargo-home"), CARGO_TARGET_DIR: path.join(scratch, "target"), CARGO_TERM_COLOR: "never", CARGO_REGISTRIES_CRATES_IO_PROTOCOL: "sparse", NODE_OPTIONS: "" });
  if (registry === "crates") Object.assign(child, { GITHUB_ACTIONS: "true", RUNNER_TEMP: env.RUNNER_TEMP });
  return child;
}

export async function consumeReadback({ env, bundle, output, expectedHash, root = ROOT, approvedFile = APPROVED_ARTIFACTS_FILE, fetchImpl = fetch, spawn = spawnSync }) {
  need(env.GITHUB_ACTIONS === "true" && env.GITHUB_REF === "refs/heads/main" && env.GITHUB_REPOSITORY === REPOSITORY && env.GITHUB_WORKFLOW_REF === `${REPOSITORY}/${READBACK}@refs/heads/main` && env.RUNNER_ENVIRONMENT === "github-hosted" && ((env.RUNNER_OS === "Windows" && process.platform === "win32") || (env.RUNNER_OS === "macOS" && process.platform === "darwin")), "native main hosted Actions required");
  for (const [key, value] of Object.entries(env)) need(!value || !/^(?:NODE_AUTH_TOKEN|NPM_TOKEN|CARGO_REGISTRY_TOKEN|GITHUB_TOKEN|GH_TOKEN|ACTIONS_ID_TOKEN_REQUEST_(?:TOKEN|URL))$/i.test(key), "consumer must be credential/OIDC free");
  verifyBundle(bundle, expectedHash, env);
  const evidence = read(path.join(bundle, "evidence.json")); const candidate = selectCandidate(approvedFile, evidence.candidate.id);
  need(same(candidate, evidence.candidate), "transfer candidate differs from committed approval");
  const sealed = loadSealed({ packDir: path.join(bundle, "packed"), registry: candidate.registry, unitsText: candidate.units.join(" "), run: { runId: candidate.runId, runAttempt: candidate.runAttempt } });
  need(sealed.problems.length === 0 && sealed.manifestSha256 === candidate.manifestSha256, "sealed transfer manifest/archive mismatch");
  const units = validatePublication(evidence.publicationReceipt, sealed.manifest, candidate, evidence.run);
  fs.mkdirSync(output, { recursive: true });
  const report = { schema: 1, kind: "reviewed-registry-readback", verified: false, candidateId: candidate.id, manifestSha256: candidate.manifestSha256, promotionRunId: String(evidence.run.id), promotionAttempt: evidence.run.run_attempt, promotionCommit: evidence.run.head_sha, publicationArtifact: evidence.artifact, publicationReceiptSha256: evidence.receiptSha256, runId: env.GITHUB_RUN_ID, runAttempt: env.GITHUB_RUN_ATTEMPT, commit: env.GITHUB_SHA, host: env.RUNNER_OS, transferSha256: expectedHash, units: [] };
  try {
    for (const unit of units) {
      const template = recipeDirectory(root, unit, candidate.registry);
      // Same reviewed runs & harness-owned env providers as R2a; a missing prerequisite refuses before any fetch.
      const plan = candidate.registry === "crates" ? planRuns({ runs: cargoRecipe({ root, unit }).runs, env, scratchRoot: fs.realpathSync(env.RUNNER_TEMP), platform: process.platform }) : null;
      const helper = plan ? planNativeHelper({ unit, plan, env, platform: process.platform }) : null;
      const archive = await registryArchive(unit, candidate.registry, fetchImpl);
      const scratch = fs.mkdtempSync(path.join(env.RUNNER_TEMP, "rk-r2b-")); const consumer = path.join(scratch, "consumer");
      fs.cpSync(template, consumer, { recursive: true });
      const child = readbackChildEnv(env, scratch, candidate.registry);
      for (const dir of [child.HOME, child.CARGO_HOME]) fs.mkdirSync(dir);
      const result = { name: unit.name, version: unit.version, archiveSha256: archive.sha256, commands: [], verified: false };
      report.units.push(result); write(path.join(output, `${unit.name.replaceAll("/", "-").replace("@", "")}-registry.json`), archive.metadata);
      const run = (command, args, shell = false, extraEnv = {}) => {
        const r = spawn(command, args, { cwd: consumer, env: { ...child, ...extraEnv }, encoding: "utf8", timeout: 30 * 60_000, maxBuffer: 32 * 1024 * 1024, windowsHide: true, shell });
        result.commands.push({ command, args, status: r.status, stdout: String(r.stdout ?? ""), stderr: String(r.stderr ?? "") });
        need(!r.error && r.status === 0, `${unit.name} consumer command failed`);
        return String(r.stdout ?? "").trim();
      };
      if (candidate.registry === "crates") {
        // Match qualification's SHA-pinned setup action & its selected/default toolchain.
        // This repository has no compiler version pin; report actual executables used.
        if (env.RUSTUP_TOOLCHAIN) child.RUSTUP_TOOLCHAIN = env.RUSTUP_TOOLCHAIN;
        result.toolchain = { rustc: run("rustc", ["--version"]), cargo: run("cargo", ["--version"]) };
        run("cargo", ["generate-lockfile"]);
        const lockFile = path.join(consumer, "Cargo.lock"); const lock = fs.readFileSync(lockFile, "utf8");
        result.lockEvidence = cargoLockEvidence(lock, units, unit);
        run("cargo", ["fetch", "--locked"]);
        // Cargo's own downloaded registry cache must contain the exact original archive.
        const caches = path.join(child.CARGO_HOME, "registry/cache");
        const archives = fs.readdirSync(caches).map(dir => path.join(caches, dir, `${unit.name}-${unit.version}.crate`)).filter(file => fs.existsSync(file));
        need(archives.length === 1 && hash(fs.readFileSync(archives[0])) === unit.sha256, "Cargo fetched archive differs from original sealed archive");
        run("cargo", ["build", "--locked", "--offline"]);
        result.runs = plan.map(({ values, implicit, ...entry }) => entry);
        // The default-feature binary exists only until the SQLCipher variant overwrites target: stage the signed helper bundle now.
        const staged = helper && stageNativeHelper({ recipeDirectory: template, reviewedFiles: cargoRecipe({ root, unit }).evidence.files, scratch, targetDir: child.CARGO_TARGET_DIR, sourceApp: helper.sourceApp, run: (command, args, extraEnv) => run(command, args, false, extraEnv) });
        if (staged) { result.helperInput = staged.input; result.helperInputSha256 = staged.inputSha256; write(path.join(output, `${unit.name}-helper-input.json`), staged.input); }
        executeRuns({ plan, evidence: result.runs, execute: nativeHelperExecute({ helper, staged, cargo: (cargoArgs, values) => run("cargo", cargoArgs, false, values), launch: (executable, args, values) => run(executable, args, false, values) }) });
        need(fs.readFileSync(lockFile, "utf8") === lock, "Cargo lock changed during consumer run");
        write(path.join(output, `${unit.name}-lock.json`), { sha256: hash(lock), lock });
      } else {
        const config = path.join(scratch, "npmrc"); const global = path.join(scratch, "global-npmrc"); fs.writeFileSync(config, ""); fs.writeFileSync(global, "");
        Object.assign(child, { npm_config_userconfig: config, npm_config_globalconfig: global, npm_config_cache: path.join(scratch, "npm-cache"), npm_config_registry: "https://registry.npmjs.org", npm_config_update_notifier: "false", npm_config_fetch_retries: "0" });
        // npm.cmd requires shell on Windows; every argument here is a fixed literal.
        run(process.platform === "win32" ? "npm.cmd" : "npm", ["install", "--ignore-scripts", "--no-audit", "--no-fund"], process.platform === "win32");
        const lockFile = path.join(consumer, "package-lock.json"); const lock = fs.readFileSync(lockFile);
        const parsedLock = JSON.parse(lock);
        result.lockEvidence = lockEvidence(parsedLock, unit, archive.metadata);
        for (const sealedUnit of units) {
          const installed = parsedLock.packages[`node_modules/${sealedUnit.name}`];
          if (installed) need(installed.version === sealedUnit.version && installed.integrity === sealedUnit.integrity, `npm sealed closure identity mismatch: ${sealedUnit.name}`);
        }
        run(process.execPath, ["consumer.mjs"]);
        need(fs.readFileSync(lockFile).equals(lock), "npm lock changed during consumer run");
        write(path.join(output, `${unit.name.replaceAll("/", "-").replace("@", "")}-lock.json`), JSON.parse(lock));
      }
      result.verified = true;
    }
    report.verified = true; return report;
  } catch (error) { report.error = error.message; throw error; }
  finally { write(path.join(output, "reviewed-registry-readback-receipt.json"), report); }
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const env = process.env;
  if (process.argv[2] === "prepare") await prepareReadback({ env, candidateId: env.READBACK_CANDIDATE_ID, runId: env.READBACK_PROMOTION_RUN, bundle: env.READBACK_BUNDLE });
  else if (process.argv[2] === "consume") await consumeReadback({ env, bundle: env.READBACK_BUNDLE, output: env.READBACK_OUTPUT, expectedHash: env.READBACK_TRANSFER_SHA256 });
  else throw new Error("expected prepare or consume");
}
