// Pure/mocked source tests only. Root integration owns test execution.
import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { createHash } from "node:crypto";
import { authenticatePromotion, publicationArtifact, validatePublication, registryArchive, cargoLockEvidence, recipeDirectory, verifyBundle, prepareReadback, readbackChildEnv } from "./reviewed-registry-readback.mjs";
import { parseRuns, planRuns, executeRuns } from "./consumer-recipes.mjs";

const digest = b => createHash("sha256").update(b).digest("hex");
const source = "a".repeat(40); const promotion = "b".repeat(40); const archive = Buffer.from("sealed crate"); const archiveHash = digest(archive);
const candidate = { id: "logs-034", sourceSha: source, registry: "crates", manifestSha256: "c".repeat(64), runId: "10", runAttempt: "1", units: ["rightkit-logs"] };
const unit = { name: "rightkit-logs", version: "0.3.4", file: "rightkit-logs-0.3.4.crate", sha256: archiveHash };
function evidence() {
  const repository = { id: 1, full_name: "bogusyogi/rightkit", private: false, default_branch: "main" };
  const workflow = { id: 2, path: ".github/workflows/publish-reviewed.yml", state: "active" };
  const run = { id: 20, run_attempt: 2, workflow_id: 2, path: workflow.path, repository, head_repository: repository, head_sha: promotion, head_branch: "main", event: "workflow_dispatch", status: "completed", conclusion: "success" };
  const jobs = { total_count: 2, jobs: ["seal-check", "upload"].map((name, i) => ({ id: 40 + i, name, run_id: 20, run_attempt: 2, head_sha: promotion, status: "completed", conclusion: "success", started_at: "2026-10-20T00:00:00Z", completed_at: "2026-10-20T00:10:00Z", steps: name === "upload" ? [{ name: "Reauthenticate candidate & publish original Cargo archives", status: "completed", conclusion: "success" }, { name: "Keep publication receipt", conclusion: "success" }] : [] })) };
  const comparison = { status: "ahead", merge_base_commit: { sha: source } };
  return { candidate: structuredClone(candidate), repository, workflow, run, jobs, comparison, runId: "20" };
}
function publication() {
  const run = evidence().run;
  const receipt = { schema: 1, repository: "bogusyogi/rightkit", registry: "crates", dryRun: false, commit: source, promotionCommit: promotion, runId: "20", runAttempt: "2", candidate: structuredClone(candidate), receipts: [{ unit: unit.name, version: unit.version, artifact: unit.file, sha256: unit.sha256, published: true, registryVerified: true, dryRun: false, registry: "crates", commit: source, attempt: "2", outcome: "published" }] };
  const manifest = { registry: "crates", commit: source, runId: "10", runAttempt: "1", units: [structuredClone(unit)] };
  return { run, receipt, manifest };
}

test("authenticates exact public main promotion & successful upload attempt", () => {
  assert.equal(authenticatePromotion(evidence()).name, "upload");
});
test("rejects foreign provenance, source, old workflow, job attempts & skipped steps", () => {
  for (const change of [e => { e.repository.private = true; }, e => { e.run.head_repository = { id: 9, full_name: "other/repo" }; }, e => { e.run.path = ".github/workflows/publish.yml"; }, e => { e.run.event = "pull_request"; }, e => { e.comparison.status = "diverged"; }, e => { e.jobs.jobs[1].run_attempt = 1; }, e => { e.jobs.jobs[1].conclusion = "failure"; }, e => { e.jobs.jobs[1].steps[0].conclusion = "skipped"; }, e => { e.jobs.total_count = 3; }, e => { e.runId = "../../20"; }]) {
    const e = evidence(); change(e); assert.throws(() => authenticatePromotion(e));
  }
});
test("publication artifact must belong to exact upload job time window", () => {
  const e = evidence(); const upload = e.jobs.jobs[1];
  const artifact = { id: 80, name: "publication-receipt", expired: false, digest: `sha256:${"d".repeat(64)}`, created_at: "2026-10-20T00:05:00Z", workflow_run: { id: 20, head_sha: promotion, head_branch: "main", repository_id: 1, head_repository_id: 1 } };
  assert.equal(publicationArtifact(artifact, e.repository, e.run, upload), artifact);
  for (const change of [a => { a.workflow_run.head_repository_id = 9; }, a => { a.created_at = "2026-10-19T00:00:00Z"; }, a => { a.digest = null; }, a => { a.expired = true; }]) { const a = structuredClone(artifact); change(a); assert.throws(() => publicationArtifact(a, e.repository, e.run, upload)); }
});
test("receipt binds candidate ID, original manifest, every version/hash & actual promotion", () => {
  let p = publication(); assert.deepEqual(validatePublication(p.receipt, p.manifest, candidate, p.run), [unit]);
  for (const change of [p => { p.receipt.candidate.id = "foreign"; }, p => { p.receipt.candidate.manifestSha256 = "0".repeat(64); }, p => { p.receipt.receipts[0].sha256 = "0".repeat(64); }, p => { p.receipt.receipts[0].version = "0.3.5"; }, p => { p.receipt.receipts[0].attempt = "1"; }, p => { p.receipt.receipts[0].registryVerified = false; }, p => { p.receipt.dryRun = true; }, p => { p.receipt.promotionCommit = source; }, p => { p.manifest.units.push(unit); }]) { p = publication(); change(p); assert.throws(() => validatePublication(p.receipt, p.manifest, candidate, p.run)); }
});
test("crates index & fetched public archive must both match original bytes", async () => {
  let calls = 0;
  const mock = async url => { calls++; assert.match(String(url), /^https:\/\/(index|static)\.crates\.io\//); return String(url).includes("index.crates.io") ? new Response(`${JSON.stringify({ name: unit.name, vers: unit.version, cksum: archiveHash, yanked: false })}\n`) : new Response(archive); };
  assert.equal((await registryArchive(unit, "crates", mock)).sha256, archiveHash); assert.equal(calls, 2);
  await assert.rejects(registryArchive(unit, "crates", async () => new Response(JSON.stringify({ name: unit.name, vers: unit.version, cksum: "0".repeat(64), yanked: false }))), /checksum/);
  await assert.rejects(registryArchive(unit, "crates", async url => String(url).includes("index.") ? new Response(JSON.stringify({ name: unit.name, vers: unit.version, cksum: archiveHash, yanked: false })) : new Response("tampered")), /differs/);
});
test("npm checks sha1, sha512, archive sha256 & public tarball origin", async () => {
  const npm = { name: "@rightkit/example", version: "1.0.0", sha256: archiveHash, sha1: createHash("sha1").update(archive).digest("hex"), integrity: `sha512-${createHash("sha512").update(archive).digest("base64")}` };
  const metadata = { name: npm.name, version: npm.version, dist: { shasum: npm.sha1, integrity: npm.integrity, tarball: "https://registry.npmjs.org/@rightkit/example/-/example-1.0.0.tgz" } };
  const mock = async url => String(url).endsWith(".tgz") ? new Response(archive) : new Response(JSON.stringify(metadata));
  assert.equal((await registryArchive(npm, "npm", mock)).sha256, archiveHash);
  metadata.dist.tarball = "https://foreign.example/archive.tgz"; await assert.rejects(registryArchive(npm, "npm", mock), /non-public/);
  metadata.dist.shasum = "0".repeat(40); await assert.rejects(registryArchive(npm, "npm", mock), /checksum/);
});
test("Cargo lock requires public registry exact version/checksum & no path/git closure", () => {
  const root = '[[package]]\nname = "rightkit-logs-registry-consumer"\nversion = "0.0.0"\n';
  const dependency = `[[package]]\nname = "rightkit-logs"\nversion = "0.3.4"\nsource = "registry+https://github.com/rust-lang/crates.io-index"\nchecksum = "${archiveHash}"\n`;
  assert.equal(cargoLockEvidence(root + dependency, [unit], unit).length, 1);
  for (const lock of [root, root + dependency.replace(archiveHash, "0".repeat(64)), root + dependency.replace("registry+https://github.com/rust-lang/crates.io-index", "git+https://foreign.test"), root + dependency + '[[package]]\nname = "local"\nversion = "1.0.0"\n']) assert.throws(() => cargoLockEvidence(lock, [unit], unit));
});
test("missing recipe, npm generic fallback, paths & fixture redirects fail closed", t => {
  const temp = fs.mkdtempSync(path.join(os.tmpdir(), "r2b-source-test-")); t.after(() => fs.rmSync(temp, { recursive: true, force: true }));
  assert.throws(() => recipeDirectory(temp, unit, "crates"), /missing committed/);
  assert.throws(() => recipeDirectory(temp, { ...unit, name: "../../escape" }, "crates"), /unsafe/);
  assert.throws(() => recipeDirectory(temp, { name: "@rightkit/example", version: "1.0.0" }, "npm"), /missing committed/);
  const fixture = path.join(temp, "ci/consumers/rightkit-logs"); fs.mkdirSync(path.join(fixture, "src"), { recursive: true }); fs.writeFileSync(path.join(fixture, "src/main.rs"), "fn main() {}\n");
  fs.writeFileSync(path.join(fixture, "Cargo.toml"), '[dependencies]\nrightkit-logs = { version = "=0.3.4" }\n'); assert.equal(recipeDirectory(temp, unit, "crates"), fixture);
  fs.appendFileSync(path.join(fixture, "Cargo.toml"), 'other = { path = "../local" }\n'); assert.throws(() => recipeDirectory(temp, unit, "crates"), /path/);
});
test("transfer index bound to trusted output digest & exact current run identity", t => {
  const temp = fs.mkdtempSync(path.join(os.tmpdir(), "r2b-transfer-test-")); t.after(() => fs.rmSync(temp, { recursive: true, force: true }));
  const env = { GITHUB_RUN_ID: "200", GITHUB_RUN_ATTEMPT: "1", GITHUB_SHA: promotion };
  fs.writeFileSync(path.join(temp, "data.json"), "sealed");
  const index = JSON.stringify({ runId: "200", runAttempt: "1", commit: promotion, files: { "data.json": digest("sealed") } }); fs.writeFileSync(path.join(temp, "transfer.json"), index);
  assert.equal(verifyBundle(temp, digest(index), env).runId, "200");
  assert.throws(() => verifyBundle(temp, digest(index), { ...env, GITHUB_RUN_ATTEMPT: "2" }), /foreign/);
  assert.throws(() => verifyBundle(temp, "0".repeat(64), env), /digest/);
  fs.writeFileSync(path.join(temp, "data.json"), "tampered"); assert.throws(() => verifyBundle(temp, digest(index), env), /hash/);
});
test("dispatch paths/URLs & untrusted workflow refuse before any evidence request", async () => {
  let requested = false; const fetchImpl = async () => { requested = true; throw new Error("unexpected request"); };
  await assert.rejects(prepareReadback({ env: {}, candidateId: "logs", runId: "https://foreign.test", bundle: "unused", fetchImpl }));
  assert.equal(requested, false);
});
test("readback native workflow matches qualification setup & records actual compiler versions", () => {
  const workflow = fs.readFileSync(new URL("../../.github/workflows/reviewed-registry-readback.yml", import.meta.url), "utf8");
  const native = workflow.slice(workflow.indexOf("  consumer:"));
  assert.match(native, /os: \[xcode-27, windows-2025\]/);
  assert.doesNotMatch(native, /macos-15|rk-setup/);
  assert.match(native, /uses: dtolnay\/rust-toolchain@f8be11a05b1d4f3fcebe6410cc16743212b999b0/);
  assert.ok(native.indexOf("dtolnay/rust-toolchain@") < native.indexOf("run: node scripts/ci/reviewed-registry-readback.mjs consume"));
  const implementation = fs.readFileSync(new URL("./reviewed-registry-readback.mjs", import.meta.url), "utf8");
  assert.match(implementation, /if \(env\.RUSTUP_TOOLCHAIN\) child\.RUSTUP_TOOLCHAIN = env\.RUSTUP_TOOLCHAIN/);
  assert.match(implementation, /result\.toolchain = \{ rustc: run\("rustc", \["--version"\]\), cargo: run\("cargo", \["--version"\]\) \}/);
});
test("R2b readback executes the same reviewed runs through the shared planner, never a bare cargo run", () => {
  const implementation = fs.readFileSync(new URL("./reviewed-registry-readback.mjs", import.meta.url), "utf8");
  for (const needle of ["planRuns({ runs: cargoRecipe({ root, unit }).runs", "executeRuns({ plan, evidence: result.runs", "platform: process.platform", "{ ...child, ...extraEnv }"]) assert.ok(implementation.includes(needle), needle);
  assert.equal(implementation.includes('run("cargo", ["run", "--locked", "--offline"])'), false);
  assert.ok(implementation.indexOf("planRuns(") < implementation.indexOf("await registryArchive(unit"), "prerequisites must refuse before any registry fetch");
});
test("R2b readback child env carries GITHUB_ACTIONS & RUNNER_TEMP like R2a, no token, and a planned run sees them", t => {
  const temp = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-r2b-env-"))); t.after(() => fs.rmSync(temp, { recursive: true, force: true }));
  const scratch = path.join(temp, "scratch"); fs.mkdirSync(scratch);
  const env = { PATH: process.env.PATH, USERPROFILE: os.homedir(), RUNNER_TEMP: temp, GITHUB_ACTIONS: "true", GITHUB_TOKEN: "t1", NODE_AUTH_TOKEN: "t2", CARGO_REGISTRY_TOKEN: "t3", GH_TOKEN: "t4", ACTIONS_ID_TOKEN_REQUEST_TOKEN: "t5", ACTIONS_ID_TOKEN_REQUEST_URL: "https://oidc.test", RIGHTKIT_TEST_REAL_USER_STORES: "0", RK_CONSUMER_RUN_ROOT: "/candidate" };
  const child = readbackChildEnv(env, scratch, "crates");
  assert.equal(child.GITHUB_ACTIONS, "true"); assert.equal(child.RUNNER_TEMP, temp);
  for (const name of Object.keys(child)) assert.doesNotMatch(name, /TOKEN|^GH_|^ACTIONS_ID/, name);
  assert.deepEqual(Object.values(child).filter(v => ["t1", "t2", "t3", "t4", "t5"].includes(v)), []);
  assert.equal(child.RK_CONSUMER_RUN_ROOT, undefined); assert.equal(child.RIGHTKIT_TEST_REAL_USER_STORES, undefined);
  const runs = parseRuns([{ id: "native-db", args: ["native-db"], env: { RIGHTKIT_TEST_REAL_USER_STORES: "literal1", RK_CONSUMER_RUN_ROOT: "freshRunRoot" } }]);
  const plan = planRuns({ runs, env, scratchRoot: temp, platform: process.platform }); const seen = [];
  const evidence = plan.map(({ values, ...e }) => e);
  executeRuns({ plan, evidence, execute: (args, values) => { seen.push({ args, env: { ...child, ...values } }); return ""; } });
  const runEnv = seen[0].env;
  assert.equal(runEnv.GITHUB_ACTIONS, "true"); assert.equal(runEnv.RUNNER_TEMP, temp); assert.equal(runEnv.RIGHTKIT_TEST_REAL_USER_STORES, "1");
  assert.ok(path.relative(temp, runEnv.RK_CONSUMER_RUN_ROOT).startsWith("rk-run-root-")); assert.equal(evidence[0].status, "passed");
  assert.equal(Object.keys(runEnv).some(name => /TOKEN/.test(name)), false);
  assert.equal(readbackChildEnv(env, scratch, "npm").GITHUB_ACTIONS, undefined);
});
