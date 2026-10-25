import assert from "node:assert/strict";
import crypto from "node:crypto";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { buildCrateUploadMetadata, commandBuild, commandLedger, commandPack, commandVerify, crateIndexPath, crateSourceProblems, crateWorkspaceFromMetadata, ledgerEntries, parseAllowList, parseUnitNames, planPublication, readNpmWorkspace, satisfies, stageSealedHelperEnv } from "./publish.mjs";
import { APPROVED_ARTIFACTS_FILE, checkReceipts, linuxRequiredFor, requiredHostsFor, buildCratePublishBody, checkNpmTarball, checkUploadPayloads, CRATES_PUBLISH_URL, childEnv, executeUpload as executeUploadRaw, loadApprovedArtifacts, loadSealed, NPM_REGISTRY_URL, parseCratesResponse, repositoryProblems, waitForNpmBytes } from "./upload.mjs";
import { consumeCrates, consumeNpm as consumeNpmChecked, declaredEntries, npmCliCommand, skipImportList } from "./consume.mjs";
// These portability tests use synthetic units without a reviewed PK recipe. Refusal of unreviewed npm units is
// covered in npm-recipe-consumer.test.mjs, so only that one problem is dropped here.
const consumeNpm = (options) => { const rows = consumeNpmChecked(options); for (const row of rows.values()) row.problems = row.problems.filter((problem) => !problem.includes("missing reviewed PK example")); return rows; };
import { scanArchiveForSecrets } from "./archive-secret-scan.mjs";
import { assertPublishDoc, assertUploadScript, CREDENTIALS_OK_OR_SKIPPED, LINUX_CHROME_IF, LINUX_IF, MUTATIONS, proveMutations, proveUploadScriptMutations, CHROME_IF, PACK_IF, PUBLISH_CONCURRENCY_GROUP, QUALIFIED_ENV, QUALIFIED_RUN, SWAP_OK_OR_SKIPPED, UNSIGNED_ARTIFACT, UNSIGNED_VALIDATE_RUN, SIGN_SWAP_IF, UPLOAD_SCRIPT_MUTATIONS, VERIFY_IF } from "./workflow-contract.mjs";
import { NATIVE_HELPER } from "./consumer-recipes.mjs";
import { pnpmBuildClosureArgs } from "./select.mjs";
import { pinFor, recordPin } from "./provision-chrome.mjs";
import { parseYaml } from "./workflow-yaml.mjs";
import { assertReviewedDoc, REVIEWED_MUTATIONS } from "./reviewed-workflow-contract.mjs";
import { prepareCandidate, authenticateAdmission } from "./reviewed-candidate.mjs";

// A directory for tar's -C. Git for Windows puts a GNU tar on PATH that reads a backslash as an escape, so
// "C:\\x\\y" is not found; both it and the system bsdtar accept forward slashes.
const tarDir = (dir) => (process.platform === "win32" ? dir.replaceAll("\\", "/") : dir);

const SHA = "a".repeat(40);
const sha256 = (bytes) => crypto.createHash("sha256").update(bytes).digest("hex");
const tmp = (label) => fs.mkdtempSync(path.join(os.tmpdir(), `rk-${label}-`));

// The uploader refuses any sealed unit without a reviewer-approved entry (ci/approved-artifacts.json). Tests that are
// not about approval run with a list that approves exactly what the manifest in `packDir` seals, read at call time so a
// tarball tampered after sealing is still caught by its own checks. Approval tests call executeUploadRaw directly.
const approvedEntry = (registry, unit, over = {}) => ({ registry, name: unit.name, version: unit.version, sha256: unit.sha256, reviewer: "RK Astra (fixture)", reviewRef: "pulse:fixture", ...over });
function approvedFor(packDir) {
  let manifest = { registry: "npm", units: [] };
  try { manifest = JSON.parse(fs.readFileSync(path.join(packDir, "manifest.json"), "utf8")); } catch { /* the upload refuses an unreadable manifest on its own */ }
  const file = path.join(tmp("approved"), "approved-artifacts.json");
  fs.writeFileSync(file, JSON.stringify({ schema: 1, artifacts: (manifest.units ?? []).map((u) => approvedEntry(manifest.registry, u)) }));
  return file;
}
const executeUpload = async (options) => {
  const input = { approvedFile: approvedFor(options.packDir), ...options };
  // Keep malformed-fixture checks on the public uploader. Only otherwise-valid real
  // uploads advance through the same preparation & admission APIs as promotion.
  try { return await executeUploadRaw(input); } catch (error) {
    if (error.message !== "real upload requires an admitted authenticated candidate context") throw error;
  }
  return executeUploadRaw({ ...input, ...await admittedUploadFixture(input) });
};

function admissionZip(files) {
  const rows = Object.entries(files).map(([name, bytes]) => {
    const filename = Buffer.from(name), header = Buffer.alloc(46);
    header.writeUInt32LE(0x02014b50); header.writeUInt16LE(3 << 8, 4);
    header.writeUInt32LE(bytes.length, 24); header.writeUInt16LE(filename.length, 28);
    header.writeUInt32LE((0x81a4 << 16) >>> 0, 38);
    return Buffer.concat([header, filename]);
  });
  const payload = Buffer.concat(Object.values(files)), directory = Buffer.concat(rows), end = Buffer.alloc(22);
  end.writeUInt32LE(0x06054b50); end.writeUInt16LE(rows.length, 8); end.writeUInt16LE(rows.length, 10);
  end.writeUInt32LE(directory.length, 12); end.writeUInt32LE(payload.length, 16);
  return Buffer.concat([payload, directory, end]);
}

async function admittedUploadFixture(input) {
  const manifestBytes = fs.readFileSync(path.join(input.packDir, "manifest.json"));
  const manifest = JSON.parse(manifestBytes);
  const root = tmp("admission");
  const files = { packed: Object.fromEntries(fs.readdirSync(input.packDir).map((name) => [name, fs.readFileSync(path.join(input.packDir, name))])) };
  for (const host of ["macos", "windows"]) {
    const name = `verify-${host}.json`;
    files[host] = { [name]: fs.readFileSync(path.join(input.receiptsDir, `publish-receipt-verify-${host}`, name)) };
  }
  const candidate = { id: "transport-fixture", sourceSha: manifest.commit, runId: String(manifest.runId), runAttempt: String(manifest.runAttempt), workflowId: "5", manifestSha256: sha256(manifestBytes), registry: manifest.registry, units: manifest.units.map((u) => u.name), allow: Object.fromEntries(Object.entries({ hostExclusion: "HOST_EXCLUSION", noTest: "NO_TEST", ignored: "IGNORED", missingAssets: "MISSING_ASSETS" }).map(([key, suffix]) => [key, (input.env[`PUBLISH_ALLOW_${suffix}`] ?? "").split(/\s+/).filter(Boolean)])), artifacts: {} };
  const archives = {}, artifacts = {};
  const repo = { id: 99, full_name: "bogusyogi/rightkit", default_branch: "main" };
  const promotionSha = "b".repeat(40), promotionId = "88";
  const artifact = (id, name, bytes, runId, sourceSha) => ({ id, name, expired: false, digest: `sha256:${sha256(bytes)}`, workflow_run: { id: Number(runId), head_sha: sourceSha, head_branch: "main", repository_id: repo.id, head_repository_id: repo.id } });
  for (const [index, host] of ["packed", "macos", "windows"].entries()) {
    const id = 100 + index; archives[id] = admissionZip(files[host]);
    artifacts[id] = artifact(id, host === "packed" ? "publish-packed" : `publish-receipt-verify-${host === "macos" ? "xcode-27" : "windows-2025"}`, archives[id], candidate.runId, candidate.sourceSha);
    candidate.artifacts[host] = { id: String(id), digest: artifacts[id].digest, ...(host === "packed" ? {} : { receiptSha256: sha256(Object.values(files[host])[0]) }) };
  }
  const admission = { schema: 1, kind: "semantic-admission", original: { candidateId: candidate.id, candidateSha256: sha256(JSON.stringify(candidate)), sourceSha: candidate.sourceSha, manifestSha256: candidate.manifestSha256, archives: manifest.units.map(({ name, version, file, sha256, metaFile, metaSha256 }) => ({ name, version, file, sha256, ...(metaFile ? { metaFile, metaSha256 } : {}) })) }, sourceTree: "c".repeat(40), sourceHashes: { "fixture.json": "d".repeat(64) }, verifierSha: promotionSha, runId: promotionId, runAttempt: input.env.GITHUB_RUN_ATTEMPT, qa: Object.fromEntries(["macos", "windows"].map((host, i) => [host, { id: String(300 + i), digest: `sha256:${"e".repeat(64)}`, summarySha256: "f".repeat(64), unitEvidenceSha256: "a".repeat(64) }])), units: manifest.units.map((u) => ({ name: u.name, version: u.version, archiveSha256: u.sha256, ok: true, policies: { macos: "required-positive-tests", windows: "required-positive-tests" }, failures: [], adoptionPending: [], runtime: manifest.registry === "crates" ? { status: "ok" } : null, runtimeFeatureProof: manifest.registry === "crates" ? { classification: "false", listedOwner: false, required: false, status: "not-applicable", failures: [] } : null, runtimeInputHashes: {}, evidenceSha256: "b".repeat(64) })) };
  files.admission = { "admission.json": Buffer.from(JSON.stringify(admission)) };
  archives[400] = admissionZip(files.admission);
  artifacts[400] = artifact(400, "reviewed-admission-receipt", archives[400], promotionId, promotionSha);
  const approvedFile = path.join(root, "approved.json"), supplementalFile = path.join(root, "supplements.json");
  fs.writeFileSync(approvedFile, JSON.stringify({ schema: 2, candidates: [candidate], artifacts: JSON.parse(fs.readFileSync(input.approvedFile)).artifacts }));
  fs.writeFileSync(supplementalFile, JSON.stringify({ schema: 1, supplements: [] }));
  const env = { ...input.env, GITHUB_SHA: promotionSha, GITHUB_RUN_ID: promotionId, GITHUB_TOKEN: "read-fixture", GITHUB_WORKFLOW_REF: `${repo.full_name}/.github/workflows/publish-reviewed.yml@refs/heads/main`, RUNNER_TEMP: root, PUBLISH_ADMISSION_ID: "400", PUBLISH_ADMISSION_DIGEST: artifacts[400].digest };
  const workflow = (promotion) => ({ id: promotion ? 7 : 5, path: `.github/workflows/${promotion ? "publish-reviewed" : "publish"}.yml`, state: "active" });
  const run = (promotion) => ({ id: Number(promotion ? promotionId : candidate.runId), run_attempt: Number(promotion ? env.GITHUB_RUN_ATTEMPT : candidate.runAttempt), repository: repo, head_repository: repo, workflow_id: promotion ? 7 : 5, path: workflow(promotion).path, event: "workflow_dispatch", head_branch: "main", head_sha: promotion ? promotionSha : candidate.sourceSha, status: promotion ? "in_progress" : "completed", conclusion: "success" });
  const fetchImpl = async (url) => {
    const route = String(url).slice(`https://api.github.com/repos/${repo.full_name}`.length).replace(/^\//, "");
    let doc;
    if (!route) doc = repo;
    else if (route.startsWith("actions/workflows/")) doc = workflow(route.includes("publish-reviewed"));
    else if (route.startsWith("compare/")) doc = { status: "ahead", merge_base_commit: { sha: candidate.sourceSha } };
    else if (route.startsWith("actions/runs/")) {
      const promotion = route.split("/")[2] === promotionId;
      if (route.includes("/jobs?")) { const r = run(promotion); const names = promotion ? ["seal-check"] : ["guard", "plan", "pack", "verify (xcode-27, 26.8.1)", "verify (windows-2025, 26.8.1)"]; doc = { total_count: names.length, jobs: names.map((name) => ({ name, run_id: r.id, run_attempt: r.run_attempt, head_sha: r.head_sha, status: "completed", conclusion: "success" })) }; }
      else doc = run(promotion);
    } else if (/^actions\/artifacts\/\d+/.test(route)) {
      const id = route.split("/")[2];
      if (route.endsWith("/zip")) return { ok: true, status: 200, arrayBuffer: async () => archives[id] };
      doc = artifacts[id];
    } else throw new Error(`unexpected admission fixture API ${route}`);
    return { ok: true, status: 200, json: async () => doc };
  };
  const spawn = (command, args) => {
    assert.equal(command, "unzip");
    const host = path.basename(args[1]).replace(/\.zip$/, "");
    return { status: 0, stdout: args[0] === "-Z1" ? Object.keys(files[host]).join("\n") : files[host][args[2]] };
  };
  const prepared = await prepareCandidate({ env, candidateId: candidate.id, approvedFile, supplementalFile, fetchImpl, spawn });
  await authenticateAdmission({ prepared, env, fetchImpl, spawn });
  return { env, packDir: prepared.packDir, receiptsDir: prepared.receiptsDir, approvedFile, candidateContext: prepared.context };
}



// ---- requirements -----------------------------------------------------------------------------

test("Cargo requirements: bare is caret, 0.x narrows, ranges and exact pins hold", () => {
  assert.equal(satisfies("0.3.4", "0.3", "cargo"), true);
  assert.equal(satisfies("0.4.0", "0.3", "cargo"), false);
  assert.equal(satisfies("1.9.0", "1.2", "cargo"), true);
  assert.equal(satisfies("2.0.0", "1.2", "cargo"), false);
  assert.equal(satisfies("0.0.3", "0.0.3", "cargo"), true);
  assert.equal(satisfies("0.0.4", "0.0.3", "cargo"), false);
  assert.equal(satisfies("0.1.0", "=0.1.0", "cargo"), true);
  assert.equal(satisfies("0.1.1", "=0.1.0", "cargo"), false);
  assert.equal(satisfies("1.4.0", ">=1.2, <1.5", "cargo"), true);
  assert.equal(satisfies("1.5.0", ">=1.2, <1.5", "cargo"), false);
});

test("Cargo requirements: wildcard, tilde, comparison and comma-list boundaries follow Cargo", () => {
  const table = [
    // wildcard: 1.2.* is >=1.2.0, <1.3.0 (not the caret of 1.2)
    ["1.2.0", "1.2.*", true], ["1.2.9", "1.2.*", true], ["1.3.0", "1.2.*", false], ["1.9.0", "1.2.*", false], ["1.1.9", "1.2.*", false],
    ["1.0.0", "1.*", true], ["1.99.0", "1.*", true], ["2.0.0", "1.*", false], ["0.9.0", "1.*", false],
    ["0.0.0", "*", true], ["9.9.9", "*", true], ["1.2.9", "1.2.x", true], ["1.3.0", "1.2.X", false],
    // tilde
    ["1.2.3", "~1.2.3", true], ["1.2.2", "~1.2.3", false], ["1.2.9", "~1.2.3", true], ["1.3.0", "~1.2.3", false],
    ["1.2.9", "~1.2", true], ["1.3.0", "~1.2", false], ["1.9.9", "~1", true], ["2.0.0", "~1", false],
    // caret
    ["0.2.9", "^0.2", true], ["0.3.0", "^0.2", false], ["0.0.9", "^0.0", true], ["0.1.0", "^0.0", false],
    ["0.9.9", "^0", true], ["1.0.0", "^0", false], ["1.9.9", "^1.2.3", true], ["2.0.0", "^1.2.3", false], ["0.0.3", "^0.0.3", true], ["0.0.4", "^0.0.3", false],
    // exact and partial equality
    ["1.2.9", "=1.2", true], ["1.3.0", "=1.2", false], ["1.9.9", "=1", true], ["2.0.0", "=1", false],
    // comparison: partial versions widen the way Cargo's semver does
    ["1.2.9", ">1.2", false], ["1.3.0", ">1.2", true], ["1.9.9", ">1", false], ["2.0.0", ">1", true],
    ["1.1.9", "<1.2", true], ["1.2.0", "<1.2", false], ["1.2.9", "<=1.2", true], ["1.3.0", "<=1.2", false],
    ["1.2.0", ">=1.2", true], ["1.1.9", ">=1.2", false], ["1.2.3", ">=1.2.3", true], ["1.2.3", ">1.2.3", false],
    // comma lists, with and without spaces, and operators followed by a space
    ["1.2.5", ">=1.2.0, <1.2.5", false], ["1.2.4", ">=1.2.0,<1.2.5", true], ["1.2.4", ">= 1.2.0 , < 1.2.5", true], ["1.0.0", ">=1.2.0, <1.2.5", false],
    ["1.4.0", ">=1, <1.5, ~1.4", true], ["1.5.0", ">=1, <1.5, ~1.4", false],
  ];
  for (const [version, requirement, expected] of table) assert.equal(satisfies(version, requirement, "cargo"), expected, `${version} against ${requirement}`);
  for (const bad of ["1.*.3", "1.0.0-beta.1", "1.0 || 2.0", "^", ">=", "1.2.3.4"]) assert.throws(() => satisfies("1.0.0", bad, "cargo"), /unsupported/, bad);
  assert.equal(satisfies("1.0.0-beta.1", "*", "cargo"), false);
});

test("npm requirements: bare is exact, ranges join with spaces and ||", () => {
  assert.equal(satisfies("0.1.6", "0.1.6", "npm"), true);
  assert.equal(satisfies("0.1.7", "0.1.6", "npm"), false);
  assert.equal(satisfies("0.2.5", ">=0.2.0 <1", "npm"), true);
  assert.equal(satisfies("1.0.0", ">=0.2.0 <1", "npm"), false);
  assert.equal(satisfies("0.2.5", "^0.2.5", "npm"), true);
  assert.equal(satisfies("0.3.0", "^0.2.5", "npm"), false);
  assert.equal(satisfies("2.1.0", "^1.0.0 || ^2.0.0", "npm"), true);
  assert.equal(satisfies("1.9.0", "1.2.x", "npm"), false);
  assert.equal(satisfies("1.2.7", "1.2.x", "npm"), true);
  assert.equal(satisfies("0.1.0-beta.1", "*", "npm"), false);
  assert.throws(() => satisfies("1.0.0", "1.0.0-beta.1", "npm"), /unsupported/);
});

test("unit names are matched strictly", () => {
  assert.deepEqual(parseUnitNames("rightkit-fs  rightkit-job-queue rightkit-fs", "crates"), ["rightkit-fs", "rightkit-job-queue"]);
  assert.deepEqual(parseUnitNames("@rightkit/logs", "npm"), ["@rightkit/logs"]);
  for (const bad of ["", "  ", "rightkit-fs;rm", "$(id)", "Rightkit", "../x", "a`b`"]) assert.throws(() => parseUnitNames(bad, "crates"), /empty|valid/);
  assert.throws(() => parseUnitNames("@rightkit/logs --tag evil", "npm"), /valid/);
  assert.throws(() => parseUnitNames("a ".repeat(100), "crates"), /limit/);
  assert.equal(crateIndexPath("rightkit-fs"), "ri/gh/rightkit-fs");
  assert.equal(crateIndexPath("ab"), "2/ab");
  assert.deepEqual(parseAllowList("rightkit-a", "crates", ["rightkit-a", "rightkit-b"], "x"), ["rightkit-a"]);
  assert.throws(() => parseAllowList("rightkit-zzz", "crates", ["rightkit-a"], "allow_host_exclusion"), /allow_host_exclusion names "rightkit-zzz"/);
});

// ---- Cargo metadata -> workspace (findings 7 and 8) -------------------------------------------

const metaDep = (name, req, extra = {}) => ({ name, req, kind: null, optional: false, source: "registry+https://github.com/rust-lang/crates.io-index", rename: null, ...extra });
const metaPkg = (name, version, dependencies = [], extra = {}) => ({ id: `path+file:///ws/crates/${name}#${version}`, name, version, manifest_path: `/ws/crates/${name}/Cargo.toml`, publish: null, dependencies, ...extra });

test("cargo metadata is the dependency source: quoted keys, inherited and renamed dependencies, targets and dev edges all arrive", () => {
  const meta = {
    workspace_root: "/ws",
    packages: [
      metaPkg("rightkit-top", "0.3.0", [
        metaDep("rightkit-base", "^0.2", { source: null }), // `"rightkit-base" = { path = ".." , version = "0.2" }`: a quoted key
        metaDep("rightkit-inherited", "=0.1.0", { source: null }), // `rightkit-inherited.workspace = true`, resolved by Cargo
        metaDep("rightkit-real", "0.4", { source: null, rename: "alias" }), // `alias = { package = "rightkit-real", ... }`
        metaDep("rightkit-win", "0.1", { source: null, optional: true }),
        metaDep("rightkit-devonly", "*", { source: null, kind: "dev" }),
        metaDep("serde", "^1"),
      ]),
      metaPkg("rightkit-base", "0.2.0"), metaPkg("rightkit-inherited", "0.1.0"), metaPkg("rightkit-real", "0.4.2"), metaPkg("rightkit-win", "0.1.0"), metaPkg("rightkit-devonly", "0.1.0"),
      metaPkg("rightkit-private", "0.1.0", [], { publish: [] }),
      metaPkg("rightkit-other-registry", "0.1.0", [], { publish: ["corp"] }),
      metaPkg("rightkit-explicit", "0.1.0", [], { publish: ["crates-io"] }),
    ],
  };
  meta.workspace_members = meta.packages.map((p) => p.id);
  const ws = crateWorkspaceFromMetadata(meta);
  const top = ws.get("rightkit-top");
  assert.equal(top.dir, "crates/rightkit-top");
  const dep = (name) => top.deps.find((d) => d.name === name);
  assert.equal(dep("rightkit-base").spec, "^0.2");
  assert.equal(dep("rightkit-inherited").spec, "=0.1.0");
  assert.equal(dep("rightkit-real").rename, "alias");
  assert.equal(dep("rightkit-real").spec, "0.4");
  assert.equal(dep("rightkit-win").optional, true);
  assert.equal(dep("rightkit-devonly").dev, true);
  assert.equal(dep("rightkit-devonly").spec, null); // path-only dev-dependency
  assert.equal(dep("serde").path, false);
  assert.equal(ws.get("rightkit-private").private, true);
  assert.equal(ws.get("rightkit-other-registry").private, true);
  assert.equal(ws.get("rightkit-explicit").private, false);
  assert.throws(() => crateWorkspaceFromMetadata({ packages: [] }), /no packages/);
  assert.throws(() => crateWorkspaceFromMetadata({ workspace_root: "/ws", packages: [{ ...metaPkg("x", "1.0.0"), dependencies: [{ name: "y" }] }] }), /without a name or requirement/);
});

// ---- planning against a fixture workspace and a fake registry ---------------------------------

function fixture(redKeys = {}, rust = { "rightkit-win": { os: ["windows"] }, "rightkit-maconly": { os: ["macos"] } }) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "rk-publish-test-"));
  fs.mkdirSync(path.join(root, "ci"), { recursive: true });
  fs.writeFileSync(path.join(root, "ci", "known-red.json"), JSON.stringify({ macos: redKeys, windows: {} }));
  fs.writeFileSync(path.join(root, "ci", "gate.config.json"), JSON.stringify({ rust }));
  return root;
}
const crate = (name, version, deps = [], extra = {}) => [name, { name, version, dir: `crates/${name}`, private: false, deps, ...extra }];
const dep = (name, spec, extra = {}) => ({ name, spec, dev: false, path: true, optional: false, ...extra });
function registry(entries) {
  return async (url) => {
    const hit = Object.entries(entries).find(([name]) => url.endsWith(`/${name}`));
    if (!hit) return { status: 404, ok: false, text: async () => "" };
    return { status: 200, ok: true, text: async () => hit[1].map((v) => JSON.stringify({ vers: v.vers, yanked: v.yanked === true, cksum: "ab" })).join("\n") };
  };
}

test("plan orders dependencies first and skips versions already published", async () => {
  const workspace = new Map([
    crate("rightkit-a", "0.1.0"),
    crate("rightkit-b", "0.2.0", [dep("rightkit-a", "0.1.0")]),
    crate("rightkit-c", "0.3.0", [dep("rightkit-b", "0.2")]),
    crate("rightkit-done", "1.0.0"),
  ]);
  const plan = await planPublication({ registry: "crates", unitsText: "rightkit-c rightkit-b rightkit-a rightkit-done", dryRun: true, root: fixture(), workspace, fetchImpl: registry({ "rightkit-done": [{ vers: "1.0.0" }] }) });
  assert.deepEqual(plan.units.map((u) => u.name), ["rightkit-a", "rightkit-b", "rightkit-c"]);
  assert.deepEqual(plan.skipped, [{ name: "rightkit-done", version: "1.0.0" }]);
});

test("plan orders a renamed or inherited dependency edge: the dependent requested first cannot publish first", async () => {
  // metadata reports the real package name for `alias = { package = "rightkit-real" }`; the edge must use it.
  const workspace = new Map([
    crate("rightkit-top", "0.3.0", [dep("rightkit-real", "0.4", { rename: "alias" }), dep("rightkit-inherited", "=0.1.0")]),
    crate("rightkit-real", "0.4.2"),
    crate("rightkit-inherited", "0.1.0"),
  ]);
  const plan = await planPublication({ registry: "crates", unitsText: "rightkit-top rightkit-real rightkit-inherited", dryRun: true, root: fixture(), workspace, fetchImpl: registry({}) });
  assert.deepEqual(plan.units.map((u) => u.name), ["rightkit-real", "rightkit-inherited", "rightkit-top"]);
  await assert.rejects(planPublication({ registry: "crates", unitsText: "rightkit-top", dryRun: true, root: fixture(), workspace, fetchImpl: registry({}) }), /rightkit-real 0\.4 is not on crates\.io/);
});

test("plan applies Cargo wildcard semantics: 1.2.* does not accept 1.9.0", async () => {
  const workspace = (depVersion) => new Map([crate("rightkit-lib", depVersion), crate("rightkit-app", "0.1.0", [dep("rightkit-lib", "1.2.*")])]);
  await assert.rejects(planPublication({ registry: "crates", unitsText: "rightkit-app rightkit-lib", dryRun: true, root: fixture(), workspace: workspace("1.9.0"), fetchImpl: registry({}) }), /rightkit-lib 1\.2\.\* is not on crates\.io and this request's 1\.9\.0 does not meet it/);
  await assert.rejects(planPublication({ registry: "crates", unitsText: "rightkit-app", dryRun: true, root: fixture(), workspace: workspace("1.9.0"), fetchImpl: registry({ "rightkit-lib": [{ vers: "1.9.0" }] }) }), /not on crates\.io/);
  const ok = await planPublication({ registry: "crates", unitsText: "rightkit-app rightkit-lib", dryRun: true, root: fixture(), workspace: workspace("1.2.7"), fetchImpl: registry({}) });
  assert.deepEqual(ok.units.map((u) => u.name), ["rightkit-lib", "rightkit-app"]);
});

test("plan refuses a dependency that is neither on the registry nor in the request", async () => {
  const workspace = new Map([crate("rightkit-a", "0.1.0"), crate("rightkit-b", "0.2.0", [dep("rightkit-a", "0.1.0")])]);
  await assert.rejects(planPublication({ registry: "crates", unitsText: "rightkit-b", dryRun: true, root: fixture(), workspace, fetchImpl: registry({}) }), /rightkit-a 0\.1\.0 is not on crates\.io/);
  const ok = await planPublication({ registry: "crates", unitsText: "rightkit-b", dryRun: true, root: fixture(), workspace, fetchImpl: registry({ "rightkit-a": [{ vers: "0.1.5" }] }) });
  assert.deepEqual(ok.units.map((u) => u.name), ["rightkit-b"]);
  await assert.rejects(planPublication({ registry: "crates", unitsText: "rightkit-b", dryRun: true, root: fixture(), workspace, fetchImpl: registry({ "rightkit-a": [{ vers: "0.1.5", yanked: true }] }) }), /not on crates\.io/);
});

test("plan refuses private, publish=false, known-red, windows-only, path-only, git and unknown units", async () => {
  const workspace = new Map([
    crate("rightkit-private", "0.1.0", [], { private: true }),
    crate("rightkit-red", "0.1.0"),
    crate("rightkit-win", "0.1.0"),
    crate("rightkit-pathonly", "0.1.0", [dep("rightkit-red", null)]),
    crate("rightkit-pre", "0.1.0-beta.1"),
    crate("rightkit-git", "0.1.0", [dep("rightkit-red", "0.1", { source: "git+https://example.invalid/x" })]),
  ]);
  const root = fixture({ "rust:rightkit-red": "defect" });
  for (const [unit, pattern] of [
    ["rightkit-private", /publish = false/], ["rightkit-red", /known-red/], ["rightkit-win", /packed on the macOS runner/],
    ["rightkit-pathonly", /no version requirement/], ["rightkit-pre", /plain x\.y\.z/], ["rightkit-missing", /not a crate/], ["rightkit-git", /git dependency/],
  ]) {
    await assert.rejects(planPublication({ registry: "crates", unitsText: unit, dryRun: true, root, workspace, fetchImpl: registry({ "rightkit-red": [{ vers: "0.1.0" }] }) }), pattern, unit);
  }
});

test("a unit a host would skip, or an npm package with no tests, needs an explicit allow input", async () => {
  const workspace = new Map([crate("rightkit-maconly", "0.1.0"), crate("rightkit-plain", "0.1.0")]);
  await assert.rejects(planPublication({ registry: "crates", unitsText: "rightkit-maconly", dryRun: true, root: fixture(), workspace, fetchImpl: registry({}) }), /Windows verification would skip it; name it in allow_host_exclusion/);
  const ok = await planPublication({ registry: "crates", unitsText: "rightkit-maconly rightkit-plain", allowHostExclusionText: "rightkit-maconly", dryRun: true, root: fixture(), workspace, fetchImpl: registry({}) });
  assert.deepEqual(ok.allowHostExclusion, ["rightkit-maconly"]);
  await assert.rejects(planPublication({ registry: "crates", unitsText: "rightkit-plain", allowHostExclusionText: "rightkit-plain", dryRun: true, root: fixture(), workspace, fetchImpl: registry({}) }), /which no host excludes/);
  const pkg = (name, hasTest) => [name, { name, version: "0.1.0", dir: `packages/${name.split("/")[1]}`, private: false, hasTest, deps: [] }];
  const npm = new Map([pkg("@rightkit/ax", false), pkg("@rightkit/ui", true)]);
  const fetchImpl = async () => ({ status: 404, ok: false, text: async () => "" });
  await assert.rejects(planPublication({ registry: "npm", unitsText: "@rightkit/ax @rightkit/ui", dryRun: true, root: fixture(), workspace: npm, fetchImpl }), /@rightkit\/ax: package\.json has no test script/);
  const allowed = await planPublication({ registry: "npm", unitsText: "@rightkit/ax @rightkit/ui", allowNoTestText: "@rightkit/ax", dryRun: true, root: fixture(), workspace: npm, fetchImpl });
  assert.deepEqual(allowed.allowNoTest, ["@rightkit/ax"]);
  await assert.rejects(planPublication({ registry: "npm", unitsText: "@rightkit/ui", allowNoTestText: "@rightkit/ui", dryRun: true, root: fixture(), workspace: npm, fetchImpl }), /has a test script/);
});

test("plan detects dependency cycles and npm workspace specifiers", async () => {
  const cyc = new Map([crate("rightkit-a", "0.1.0", [dep("rightkit-b", "0.1.0")]), crate("rightkit-b", "0.1.0", [dep("rightkit-a", "0.1.0")])]);
  await assert.rejects(planPublication({ registry: "crates", unitsText: "rightkit-a rightkit-b", dryRun: true, root: fixture(), workspace: cyc, fetchImpl: registry({}) }), /cycle/);
  const pkg = (name, version, deps = [], extra = {}) => [name, { name, version, dir: `packages/${name.split("/")[1]}`, private: false, hasTest: true, deps, ...extra }];
  const npm = new Map([
    pkg("@rightkit/base", "0.2.0"),
    pkg("@rightkit/top", "0.1.0", [{ name: "@rightkit/base", spec: "workspace:*", dev: false }, { name: "@rightkit/peer", spec: ">=0.1.0 <1", dev: false }]),
    pkg("@rightkit/peer", "0.1.0"),
  ]);
  const fetchImpl = async (url) => ({ status: 404, ok: false, text: async () => "", url });
  const plan = await planPublication({ registry: "npm", unitsText: "@rightkit/top @rightkit/peer @rightkit/base", dryRun: false, root: fixture(), workspace: npm, fetchImpl });
  assert.deepEqual(plan.units.map((u) => u.name), ["@rightkit/peer", "@rightkit/base", "@rightkit/top"]); // dependencies first, ties in the order requested
  await assert.rejects(planPublication({ registry: "npm", unitsText: "@rightkit/top", dryRun: false, root: fixture(), workspace: npm, fetchImpl }), /@rightkit\/base@0\.2\.0 \(workspace version\) is not on npm/);
});

// ---- npm provenance preconditions (finding 6) -------------------------------------------------

test("every public npm package names bogusyogi/rightkit and its own directory", () => {
  const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");
  const workspace = readNpmWorkspace(root);
  assert.ok(workspace.size > 5);
  for (const unit of workspace.values()) {
    if (unit.private) continue;
    const manifest = JSON.parse(fs.readFileSync(path.join(root, unit.dir, "package.json"), "utf8"));
    assert.deepEqual(repositoryProblems(manifest, unit.dir), [], unit.name);
  }
});

test("repository problems name the missing, wrong-owner and wrong-directory cases", () => {
  const ok = { name: "@rightkit/x", repository: { type: "git", url: "git+https://github.com/bogusyogi/rightkit.git", directory: "packages/x" } };
  assert.deepEqual(repositoryProblems(ok, "packages/x"), []);
  assert.deepEqual(repositoryProblems({ ...ok, repository: { ...ok.repository, url: "https://github.com/bogusyogi/rightkit" } }, "packages/x"), []);
  assert.match(repositoryProblems({ name: "@rightkit/x" }, "packages/x")[0], /repository must be an object/);
  assert.match(repositoryProblems({ ...ok, repository: "github:bogusyogi/rightkit" }, "packages/x")[0], /repository must be an object/);
  assert.match(repositoryProblems({ ...ok, repository: { ...ok.repository, url: "git+https://github.com/someone-else/rightkit.git" } }, "packages/x").join(), /must name https:\/\/github\.com\/bogusyogi\/rightkit/);
  assert.match(repositoryProblems({ ...ok, repository: { ...ok.repository, directory: "packages/y" } }, "packages/x").join(), /directory/);
});

// ---- fixtures for sealed artifacts ------------------------------------------------------------

/** Create `<out>` as a gzip tar holding `rootName/<files>` using the real tar. */
function makeTgz(out, rootName, files) {
  const stage = tmp("stage");
  for (const [rel, content] of Object.entries(files)) {
    const full = path.join(stage, rootName, rel);
    fs.mkdirSync(path.dirname(full), { recursive: true });
    fs.writeFileSync(full, content);
  }
  const r = spawnSync("tar", ["-czf", path.basename(out), "-C", tarDir(stage), rootName], { cwd: path.dirname(out), encoding: "utf8" });
  assert.equal(r.status, 0, r.stderr);
  fs.rmSync(stage, { recursive: true, force: true });
}

const npmPackageJson = (name, dir, extra = {}) => ({ name, version: "0.1.0", main: "dist/index.js", exports: { ".": { import: "./dist/index.js" } }, repository: { type: "git", url: "git+https://github.com/bogusyogi/rightkit.git", directory: dir }, ...extra });

function sealNpm({ units = [["@rightkit/a", "packages/a"]], packageJson = npmPackageJson, extraFiles = {} } = {}) {
  const dir = tmp("sealed");
  const manifest = { schema: 1, registry: "npm", commit: SHA, runId: "77", runAttempt: "1", units: [] };
  for (const [name, unitDir] of units) {
    const file = `${name.replace(/^@/, "").replace("/", "-")}-0.1.0.tgz`;
    makeTgz(path.join(dir, file), "package", { "package.json": JSON.stringify(packageJson(name, unitDir)), "dist/index.js": "export {};\n", ...extraFiles });
    const bytes = fs.readFileSync(path.join(dir, file));
    manifest.units.push({ name, version: "0.1.0", dir: unitDir, file, size: bytes.length, sha256: sha256(bytes), sha1: crypto.createHash("sha1").update(bytes).digest("hex"), integrity: `sha512-${crypto.createHash("sha512").update(bytes).digest("base64")}` });
  }
  fs.writeFileSync(path.join(dir, "manifest.json"), JSON.stringify(manifest));
  return { dir, manifest };
}

function sealCrates(names = ["rightkit-a"]) {
  const dir = tmp("sealed-crates");
  const manifest = { schema: 1, registry: "crates", commit: SHA, runId: "77", runAttempt: "1", units: [] };
  for (const name of names) {
    const file = `${name}-0.1.0.crate`; const metaFile = `${name}-0.1.0.upload.json`;
    makeTgz(path.join(dir, file), `${name}-0.1.0`, { "Cargo.toml": `[package]\nname = "${name}"\n`, "Cargo.toml.orig": `[package]\nname = "${name}"\n`, "src/lib.rs": "pub fn a() {}\n" });
    const meta = `${JSON.stringify({ name, vers: "0.1.0", deps: [], features: {} })}\n`;
    fs.writeFileSync(path.join(dir, metaFile), meta);
    const bytes = fs.readFileSync(path.join(dir, file));
    manifest.units.push({ name, version: "0.1.0", dir: `crates/${name}`, file, size: bytes.length, sha256: sha256(bytes), metaFile, metaSha256: sha256(Buffer.from(meta)) });
  }
  fs.writeFileSync(path.join(dir, "manifest.json"), JSON.stringify(manifest));
  return { dir, manifest };
}

const archiveScanFixture = (unit) => ({ schema: 1, scanner: "publish-cargo/scanCrateForSecrets", archiveSha256: unit.sha256, status: "pass", files: 1, violations: [], problems: [] });

function receipts(packDir, manifest, { skip = [], override = {}, unitOverride = {} } = {}) {
  const dir = tmp("receipts");
  const manifestSha256 = sha256(fs.readFileSync(path.join(packDir, "manifest.json")));
  for (const host of ["macos", "windows"]) {
    if (skip.includes(host)) continue;
    fs.mkdirSync(path.join(dir, `publish-receipt-verify-${host}`), { recursive: true });
    const doc = {
      schema: 1, kind: "verify", host, registry: manifest.registry, commit: manifest.commit, manifestSha256, runId: "77", runAttempt: "1", manifestRunId: manifest.runId, manifestRunAttempt: manifest.runAttempt,
      allowHostExclusion: [], allowNoTest: [], allowIgnored: [], allowMissingAssets: [],
      units: manifest.units.map((u) => ({ name: u.name, version: u.version, file: u.file, sha256: u.sha256, archiveScan: archiveScanFixture(u), bytesVerified: true, status: "pass", executed: 3, ignored: 0, skipped: 0, consumer: { verified: true }, ...unitOverride })),
      ...override,
    };
    fs.writeFileSync(path.join(dir, `publish-receipt-verify-${host}`, `verify-${host}.json`), JSON.stringify(doc));
  }
  return dir;
}

test("both hosts require strict passing scan proof for exact sealed bytes", () => {
  const sealed = sealNpm();
  for (const host of ["macos", "windows"]) {
    for (const mutate of [
      (row) => { delete row.archiveScan; },
      (row) => { row.archiveScan.status = "fail"; },
      (row) => { row.archiveScan.archiveSha256 = "b".repeat(64); },
      (row) => { row.archiveScan.schema = 2; },
      (row) => { row.archiveScan.scanner = "other-scanner"; },
      (row) => { row.archiveScan.files = 0; },
      (row) => { row.archiveScan.files = "1"; },
      (row) => { row.archiveScan.violations = ["hidden/config"]; },
      (row) => { row.archiveScan.problems = ["unreadable archive"]; },
      (row) => { row.archiveScan.violations = {}; },
      (row) => { row.archiveScan.extra = true; },
      (row) => { row.archiveScan = []; },
    ]) {
      const receiptsDir = receipts(sealed.dir, sealed.manifest);
      const file = path.join(receiptsDir, `publish-receipt-verify-${host}`, `verify-${host}.json`);
      const receipt = JSON.parse(fs.readFileSync(file, "utf8"));
      mutate(receipt.units[0]);
      fs.writeFileSync(file, JSON.stringify(receipt));
      const problems = checkReceipts({ manifest: sealed.manifest, manifestSha256: sha256(fs.readFileSync(path.join(sealed.dir, "manifest.json"))), receiptsDir, run: { runId: "77", runAttempt: "1" } });
      assert.ok(problems.some((problem) => problem.startsWith(`${host} receipt:`) && problem.includes("archiveScan")), JSON.stringify(problems));
    }
  }
});

/** A spawn that records every call, answers the ones in `fake`, and runs tar for real. */
function spy(fake = {}) {
  const calls = [];
  const spawn = (command, args, options = {}) => {
    calls.push({ command, args: [...args], options });
    const handler = fake[command];
    if (handler) { const answer = handler(args, options, calls); if (answer !== undefined) return answer; }
    if (command === "tar" || command === "git") return spawnSync(command, args, options);
    throw new Error(`unexpected child process: ${command} ${args.join(" ")}`);
  };
  return { spawn, calls };
}

// The scans job's import-effects report (nothing INCOMPLETE): verify of npm units refuses without one.
const IMPORT_EFFECTS_REPORT = path.join(fs.mkdtempSync(path.join(os.tmpdir(), "rk-import-effects-")), "report.json");
// (the test git spy answers SHA to every rev-parse, so the checkout tree is SHA too.)
const REPORT_OK = { schema: "rightkit.import-effects/1", commit: SHA, tree: SHA, clean: true, dirty: 0, probe: "a".repeat(64), node: "v26.8.1", checked: 0, findings: 0, incomplete: [], covered: [] };
fs.writeFileSync(IMPORT_EFFECTS_REPORT, JSON.stringify(REPORT_OK));
const actionsEnv = (extra = {}) => ({ GITHUB_ACTIONS: "true", GITHUB_REF: "refs/heads/main", GITHUB_REPOSITORY: "bogusyogi/rightkit", GITHUB_SHA: SHA, GITHUB_RUN_ID: "77", GITHUB_RUN_ATTEMPT: "1", PATH: process.env.PATH ?? "", RUNNER_TEMP: os.tmpdir(), PUBLISH_REGISTRY: "npm", PUBLISH_UNITS: "@rightkit/a", RK_IMPORT_EFFECTS_REPORT: IMPORT_EFFECTS_REPORT, ...extra });

test("upload payload preflight checks npm semantics through real tar without registry access", () => {
  const cases = [
    [{}, null],
    [{ name: "@rightkit/other" }, /holds @rightkit\/other@0\.1\.0, expected/],
    [{ version: "0.2.0" }, /holds @rightkit\/a@0\.2\.0, expected/],
    [{ repository: { type: "git", url: "https://github.com/other/rightkit", directory: "packages/a" } }, /must name https:\/\/github\.com\/bogusyogi\/rightkit/],
    [{ main: "dist/missing.js" }, /points at dist\/missing\.js/],
    [{ dependencies: { "@rightkit/other": "workspace:*" } }, /workspace:/],
  ];
  for (const [extra, expected] of cases) {
    const sealed = sealNpm({ packageJson: (name, dir) => npmPackageJson(name, dir, extra) });
    const { spawn, calls } = spy();
    const problems = checkUploadPayloads({ manifest: sealed.manifest, packDir: sealed.dir, registry: "npm", spawn });
    if (expected) assert.match(problems.join("\n"), expected);
    else assert.deepEqual(problems, []);
    assert.ok(calls.some((call) => call.command === "tar" && call.args[0] === "-tzf"));
    assert.ok(calls.some((call) => call.command === "tar" && call.args[0] === "-xOzf"));
    assert.ok(calls.every((call) => call.command === "tar"), "preflight only inspects tar payloads");
  }
});

test("upload payload preflight reports unreadable npm archives & continues checking units", () => {
  const sealed = sealNpm({ units: [["@rightkit/a", "packages/a"], ["@rightkit/b", "packages/b"]], packageJson: (name, dir) => npmPackageJson(name, dir, { version: "0.2.0" }) });
  fs.writeFileSync(path.join(sealed.dir, sealed.manifest.units[0].file), "not a tarball");
  const problems = checkUploadPayloads({ manifest: sealed.manifest, packDir: sealed.dir, registry: "npm", spawn: spy().spawn });
  assert.ok(problems.some((problem) => problem.startsWith("@rightkit/a:")));
  assert.ok(problems.some((problem) => problem.includes("holds @rightkit/b@0.2.0")));
});

test("upload payload preflight checks crates metadata identity, deps shape & readability", () => {
  const sealed = sealCrates();
  const unit = sealed.manifest.units[0];
  const metaFile = path.join(sealed.dir, unit.metaFile);
  const run = () => checkUploadPayloads({ manifest: sealed.manifest, packDir: sealed.dir, registry: "crates", spawn: () => assert.fail("metadata preflight needs no child process") });
  assert.deepEqual(run(), []);
  for (const extra of [{ name: "other" }, { vers: "0.2.0" }, { deps: {} }, { deps: null }]) {
    fs.writeFileSync(metaFile, JSON.stringify({ name: unit.name, vers: unit.version, deps: [], ...extra }));
    assert.deepEqual(run(), [`${unit.name}: upload metadata does not describe ${unit.name}@${unit.version}`]);
  }
  for (const text of ["not JSON", "null"]) {
    fs.writeFileSync(metaFile, text);
    assert.deepEqual(run(), [`${unit.name}: upload metadata is unreadable`]);
  }
  fs.rmSync(metaFile);
  assert.deepEqual(run(), [`${unit.name}: upload metadata is unreadable`]);
});

const oidc = { ACTIONS_ID_TOKEN_REQUEST_URL: "https://oidc.invalid/x", ACTIONS_ID_TOKEN_REQUEST_TOKEN: "oidc-secret" };
// A registry that does not hold a unit until it is asked a second time (the first request is upload.mjs's pre-publish
// reconciliation); `already` names units that are on the registry from the start.
const npmFetch = (sealed, { already = [] } = {}) => {
  const asked = new Set();
  return async (url) => {
    const unit = sealed.manifest.units.find((u) => String(url).includes(u.name.replace("/", "%2F")));
    if (!unit) return { status: 404, ok: false, text: async () => "" };
    if (!already.includes(unit.name) && !asked.has(unit.name)) { asked.add(unit.name); return { status: 404, ok: false, text: async () => "" }; }
    return { status: 200, ok: true, text: async () => JSON.stringify({ dist: { shasum: unit.sha1, integrity: unit.integrity } }) };
  };
};
/** A stateful fake npm: `npm publish` adds the tarball's unit (unless `failing` names it), the registry API serves what was added. */
function fakeNpm(sealed, { failing = [], present = {} } = {}) {
  const held = new Map(Object.entries(present));
  const publishes = [];
  const unitOf = (text) => sealed.manifest.units.find((u) => text.includes(u.file) || text.includes(u.name.replace("/", "%2F")));
  const { spawn } = spy({ npm: (args) => {
    const unit = unitOf(path.basename(args[1]));
    publishes.push(unit.name);
    if (failing.includes(unit.name)) return { status: 1, stdout: "", stderr: "E500" };
    held.set(unit.name, { shasum: unit.sha1, integrity: unit.integrity });
    return { status: 0, stdout: "", stderr: "" };
  } });
  const fetchImpl = async (url) => {
    const unit = unitOf(String(url));
    const dist = unit && held.get(unit.name);
    if (!dist) return { status: 404, ok: false, text: async () => "" };
    return { status: 200, ok: true, text: async () => JSON.stringify({ dist }) };
  };
  return { spawn, fetchImpl, publishes, held };
}

// ---- the upload job: seal, receipts, dry run, command and environment construction ------------

test("real upload requires admitted context before registry or receipt effects", async () => {
  for (const registry of ["npm", "crates"]) {
    const sealed = registry === "npm" ? sealNpm() : sealCrates(["rightkit-a"]);
    const outDir = path.join(tmp("context-guard"), "absent"), requests = [];
    await assert.rejects(executeUploadRaw({ env: actionsEnv({ PUBLISH_REGISTRY: registry, PUBLISH_UNITS: registry === "npm" ? "@rightkit/a" : "rightkit-a", ...(registry === "npm" ? { NODE_AUTH_TOKEN: "fixture", ...oidc } : { CARGO_REGISTRY_TOKEN: "fixture" }) }), packDir: sealed.dir, receiptsDir: receipts(sealed.dir, sealed.manifest), approvedFile: approvedFor(sealed.dir), outDir, dryRun: false, spawn: spy().spawn, fetchImpl: async (...args) => { requests.push(args); throw new Error("registry must remain untouched"); } }), /requires an admitted authenticated candidate context/);
    assert.deepEqual(requests, []);
    assert.equal(fs.existsSync(outDir), false);
  }
});


test("upload (dry run) checks hashes and receipts, sends nothing, and refuses any credential", async () => {
  const sealed = sealNpm({ units: [["@rightkit/a", "packages/a"], ["@rightkit/b", "packages/b"]] });
  const receiptsDir = receipts(sealed.dir, sealed.manifest);
  const { spawn, calls } = spy();
  const lines = [];
  const base = { packDir: sealed.dir, receiptsDir, outDir: tmp("out"), dryRun: true, spawn, fetchImpl: async () => { throw new Error("a dry run must not touch the network"); }, log: (l) => lines.push(l) };
  const env = actionsEnv({ PUBLISH_UNITS: "@rightkit/a @rightkit/b" });
  const doc = await executeUpload({ ...base, env });
  assert.equal(doc.dryRun, true);
  assert.deepEqual(doc.receipts.map((r) => [r.unit, r.published]), [["@rightkit/a", false], ["@rightkit/b", false]]);
  assert.ok(calls.every((c) => c.command === "tar"), "a dry run runs no registry client");
  assert.match(lines.join("\n"), /dry run: would run npm publish rightkit-a-0\.1\.0\.tgz --ignore-scripts --access public --provenance --registry https:\/\/registry\.npmjs\.org\//);
  for (const variable of ["NODE_AUTH_TOKEN", "CARGO_REGISTRY_TOKEN"]) await assert.rejects(executeUpload({ ...base, env: { ...env, [variable]: "tok" } }), /present in a dry run/);
  await assert.rejects(executeUpload({ ...base, env: { ...env, ...oidc } }), /OIDC token is present in a dry run/);
});

test("upload (real npm) builds the publish command from the sealed tarball and isolates the credential", async () => {
  const sealed = sealNpm();
  const receiptsDir = receipts(sealed.dir, sealed.manifest);
  const outDir = tmp("out");
  const { spawn, calls } = spy({ npm: () => ({ status: 0, stdout: "+ @rightkit/a@0.1.0 with token npm-secret-token\n", stderr: "" }) });
  const lines = [];
  const env = actionsEnv({ NODE_AUTH_TOKEN: "npm-secret-token", ...oidc, GITHUB_TOKEN: "ghs_nope", CARGO_REGISTRY_TOKEN: "" , INPUT_UNITS: "x", SECRET_SAUCE: "hunter2", HOME: os.homedir() });
  const doc = await executeUpload({ env, packDir: sealed.dir, receiptsDir, outDir, dryRun: false, spawn, fetchImpl: npmFetch(sealed), log: (l) => lines.push(l), pause: 1 });
  const publish = calls.find((c) => c.command === "npm");
  assert.ok(publish, "npm publish ran");
  const file = publish.args[1];
  assert.equal(path.basename(file), sealed.manifest.units[0].file);
  assert.equal(sha256(fs.readFileSync(file)), sealed.manifest.units[0].sha256);
  assert.deepEqual(publish.args.slice(0, 7), ["publish", file, "--ignore-scripts", "--access", "public", "--provenance", "--registry"]);
  assert.equal(publish.args[7], NPM_REGISTRY_URL);
  assert.equal(publish.args[8], "--userconfig");
  assert.ok(!publish.args.some((a) => a.includes("npm-secret-token")), "the token is never on a command line");
  assert.equal(publish.options.env.NODE_AUTH_TOKEN, "npm-secret-token");
  assert.equal(publish.options.env.ACTIONS_ID_TOKEN_REQUEST_URL, oidc.ACTIONS_ID_TOKEN_REQUEST_URL);
  for (const leaked of ["SECRET_SAUCE", "INPUT_UNITS", "GITHUB_TOKEN", "CARGO_REGISTRY_TOKEN"]) assert.ok(!(leaked in publish.options.env), `${leaked} must not reach npm`);
  assert.ok(lines.join("\n").includes("<redacted>") && !lines.join("\n").includes("npm-secret-token"), "the token is redacted from output");
  assert.deepEqual(doc.receipts[0], { ...doc.receipts[0], unit: "@rightkit/a", published: true, registryVerified: true, sha256: sealed.manifest.units[0].sha256, commit: SHA, registry: "npm", dryRun: false });
  // credentials missing or crossed
  await assert.rejects(executeUpload({ env: actionsEnv({ ...oidc }), packDir: sealed.dir, receiptsDir, outDir, dryRun: false, spawn, fetchImpl: npmFetch(sealed) }), /NODE_AUTH_TOKEN is not set/);
  await assert.rejects(executeUpload({ env: actionsEnv({ NODE_AUTH_TOKEN: "npm-secret-token", CARGO_REGISTRY_TOKEN: "crate-secret-token", ...oidc }), packDir: sealed.dir, receiptsDir, outDir, dryRun: false, spawn, fetchImpl: npmFetch(sealed) }), /CARGO_REGISTRY_TOKEN must not be present/);
  await assert.rejects(executeUpload({ env: actionsEnv({ NODE_AUTH_TOKEN: "npm-secret-token" }), packDir: sealed.dir, receiptsDir, outDir, dryRun: false, spawn, fetchImpl: npmFetch(sealed) }), /needs id-token: write/);
});

test("waitForNpmBytes tolerates late npm visibility: 404 for more than the old 40 attempts, then the matching dist", async () => {
  let calls = 0;
  const fetchImpl = async () => {
    calls += 1;
    if (calls <= 45) return { status: 404, ok: false, text: async () => "" };
    return { status: 200, ok: true, text: async () => JSON.stringify({ dist: { shasum: "a".repeat(40), integrity: "sha512-x" } }) };
  };
  await waitForNpmBytes({ name: "@rightkit/a", version: "0.1.0", sha1: "a".repeat(40), integrity: "sha512-x", fetchImpl, pause: 1 });
  assert.equal(calls, 46);
});

test("waitForNpmBytes: a different shasum throws at once, without waiting for later attempts", async () => {
  let calls = 0;
  const fetchImpl = async () => {
    calls += 1;
    return { status: 200, ok: true, text: async () => JSON.stringify({ dist: { shasum: "b".repeat(40), integrity: "sha512-x" } }) };
  };
  await assert.rejects(waitForNpmBytes({ name: "@rightkit/a", version: "0.1.0", sha1: "a".repeat(40), integrity: "sha512-x", fetchImpl, pause: 1 }), /serves different bytes/);
  assert.equal(calls, 1);
});

test("upload refuses a registry that serves different bytes than the sealed tarball", async () => {
  const sealed = sealNpm();
  const { spawn } = spy({ npm: () => ({ status: 0, stdout: "", stderr: "" }) });
  const fetchImpl = async () => ({ status: 200, ok: true, text: async () => JSON.stringify({ dist: { shasum: "0".repeat(40), integrity: "sha512-x" } }) });
  await assert.rejects(executeUpload({ env: actionsEnv({ NODE_AUTH_TOKEN: "npm-secret-token", ...oidc }), packDir: sealed.dir, receiptsDir: receipts(sealed.dir, sealed.manifest), outDir: tmp("out"), dryRun: false, spawn, fetchImpl, pause: 1 }), /serves different bytes/);
});

test("upload refuses when the sealed bytes, the commit or the receipts do not bind", async () => {
  const run = (sealed, receiptsDir, env = actionsEnv({ NODE_AUTH_TOKEN: "npm-secret-token", ...oidc })) => executeUpload({ env, packDir: sealed.dir, receiptsDir, outDir: tmp("out"), dryRun: false, spawn: spy({ npm: () => ({ status: 0, stdout: "", stderr: "" }) }).spawn, fetchImpl: npmFetch(sealed), pause: 1 });
  // a tarball changed after packing
  let sealed = sealNpm();
  const receiptsDir = receipts(sealed.dir, sealed.manifest);
  fs.appendFileSync(path.join(sealed.dir, sealed.manifest.units[0].file), "tamper");
  await assert.rejects(run(sealed, receiptsDir), /sha256 does not match the manifest/);
  // an unlisted file rides along
  sealed = sealNpm();
  fs.writeFileSync(path.join(sealed.dir, "evil.sh"), "x");
  await assert.rejects(run(sealed, receipts(sealed.dir, sealed.manifest)), /unlisted file in the sealed artifact: evil\.sh/);
  // a missing host
  sealed = sealNpm();
  await assert.rejects(run(sealed, receipts(sealed.dir, sealed.manifest, { skip: ["windows"] })), /expected exactly one verify receipt from windows, found 0/);
  await assert.rejects(run(sealed, receipts(sealed.dir, sealed.manifest, { skip: ["macos"] })), /from macos, found 0/);
  // a receipt for another manifest, commit, run, or other bytes
  await assert.rejects(run(sealed, receipts(sealed.dir, sealed.manifest, { override: { manifestSha256: "f".repeat(64) } })), /different manifest/);
  await assert.rejects(run(sealed, receipts(sealed.dir, sealed.manifest, { override: { commit: "b".repeat(40) } })), /is not the sealed commit/);
  await assert.rejects(run(sealed, receipts(sealed.dir, sealed.manifest, { override: { runId: "99" } })), /is not this run 77/);
  await assert.rejects(run(sealed, receipts(sealed.dir, sealed.manifest, { unitOverride: { sha256: "e".repeat(64) } })), /verified different bytes/);
  await assert.rejects(run(sealed, receipts(sealed.dir, sealed.manifest, { unitOverride: { bytesVerified: false } })), /not verified/);
  await assert.rejects(run(sealed, receipts(sealed.dir, sealed.manifest, { unitOverride: { status: "fail" } })), /status "fail" is not a pass/);
  await assert.rejects(run(sealed, receipts(sealed.dir, sealed.manifest, { unitOverride: { status: "allowed-exclusion" } })), /without allow_host_exclusion/);
  await assert.rejects(run(sealed, receipts(sealed.dir, sealed.manifest, { unitOverride: { status: "allowed-no-test" } })), /not named in allow_no_test/);
  await assert.rejects(run(sealed, receipts(sealed.dir, sealed.manifest, { override: { allowNoTest: ["@rightkit/a"] } })), /different allow_no_test/);
  await assert.rejects(run(sealed, receipts(sealed.dir, sealed.manifest, { override: { allowIgnored: ["@rightkit/a"] } })), /different allow_ignored/);
  await assert.rejects(run(sealed, receipts(sealed.dir, sealed.manifest, { override: { allowMissingAssets: ["@rightkit/a"] } })), /different allow_missing_assets/);
  // structured results: a pass needs executed tests and nothing ignored or skipped; an allowance must be requested
  await assert.rejects(run(sealed, receipts(sealed.dir, sealed.manifest, { unitOverride: { executed: 0 } })), /without a positive executed count/);
  await assert.rejects(run(sealed, receipts(sealed.dir, sealed.manifest, { unitOverride: { skipped: 2 } })), /status pass without a positive executed count and zero ignored\/skipped/);
  await assert.rejects(run(sealed, receipts(sealed.dir, sealed.manifest, { unitOverride: { steps: [{ command: "cargo test -p x", ok: true, executed: 40 }, { command: "cargo test -p x --features y", ok: true, executed: 0 }] } })), /required test command `cargo test -p x --features y` executed no test/, "a positive total never covers a row that ran nothing");
  await run(sealed, receipts(sealed.dir, sealed.manifest, { unitOverride: { steps: [{ command: "cargo clippy -p x", ok: true }, { command: "cargo test -p x", ok: true, executed: 3 }] } })); // non-test steps carry no count
  await assert.rejects(run(sealed, receipts(sealed.dir, sealed.manifest, { unitOverride: { status: "allowed-skips", skipped: 2 } })), /not named in allow_missing_assets/);
  await assert.rejects(run(sealed, receipts(sealed.dir, sealed.manifest, { unitOverride: { status: "allowed-skips", ignored: 2 } })), /not named in allow_ignored/);
  await assert.rejects(run(sealed, receipts(sealed.dir, sealed.manifest, { unitOverride: { consumer: { verified: false } } })), /not consumer-verified/);
  // the sealed artifact and the receipts bind to THIS run and its attempts
  const unbound = JSON.parse(fs.readFileSync(path.join(sealed.dir, "manifest.json"), "utf8"));
  const rewrite = (patch) => { const d = sealNpm(); const m = { ...JSON.parse(fs.readFileSync(path.join(d.dir, "manifest.json"), "utf8")), ...patch }; fs.writeFileSync(path.join(d.dir, "manifest.json"), JSON.stringify(m)); return { dir: d.dir, manifest: m }; };
  assert.equal(unbound.runId, "77");
  let other = rewrite({ runId: "78" });
  await assert.rejects(run(other, receipts(other.dir, other.manifest)), /manifest run 78 is not this run 77/);
  other = rewrite({ runId: undefined });
  await assert.rejects(run(other, receipts(other.dir, other.manifest)), /manifest\.runId is missing/);
  other = rewrite({ runAttempt: "2" });
  await assert.rejects(run(other, receipts(other.dir, other.manifest)), /manifest run attempt 2 is later than this attempt 1/);
  await assert.rejects(run(sealed, receipts(sealed.dir, sealed.manifest, { override: { runAttempt: "2" } })), /run attempt 2 is later than this attempt 1/);
  await assert.rejects(run(sealed, receipts(sealed.dir, sealed.manifest, { override: { runAttempt: "x" } })), /not a positive integer/);
  await assert.rejects(run(sealed, receipts(sealed.dir, sealed.manifest, { override: { manifestRunAttempt: "9" } })), /it verified manifest run 77\/9/);
  const rerun = rewrite({ runAttempt: "1" });
  await assert.rejects(run(rerun, receipts(rerun.dir, rerun.manifest, { override: { runAttempt: "2" } }), actionsEnv({ GITHUB_RUN_ATTEMPT: "2", NODE_AUTH_TOKEN: "npm-secret-token", ...oidc })), /run attempt 2 is later than this attempt 1/); // admitted candidate pins original verification attempt
  await assert.rejects(run(sealed, receipts(sealed.dir, sealed.manifest), actionsEnv({ GITHUB_RUN_ID: "", NODE_AUTH_TOKEN: "npm-secret-token", ...oidc })), /GITHUB_RUN_ID and GITHUB_RUN_ATTEMPT must be set/);
  const rerunReceipts = receipts(rerun.dir, rerun.manifest, { override: { runAttempt: "1" } });
  await run(rerun, rerunReceipts, actionsEnv({ GITHUB_RUN_ATTEMPT: "2", NODE_AUTH_TOKEN: "npm-secret-token", ...oidc }));
  assert.deepEqual(loadSealed({ packDir: rerun.dir, registry: "npm", unitsText: "@rightkit/a", run: { runId: "77", runAttempt: "2" } }).problems, []);
  // wrong commit, branch, repository, unit list
  const ok = receipts(sealed.dir, sealed.manifest);
  await assert.rejects(run(sealed, ok, actionsEnv({ GITHUB_SHA: "c".repeat(40), NODE_AUTH_TOKEN: "npm-secret-token", ...oidc })), /not the dispatched commit/);
  await assert.rejects(run(sealed, ok, actionsEnv({ GITHUB_REF: "refs/heads/dev", NODE_AUTH_TOKEN: "npm-secret-token", ...oidc })), /refs\/heads\/main only/);
  await assert.rejects(run(sealed, ok, actionsEnv({ GITHUB_REPOSITORY: "someone/rightkit", NODE_AUTH_TOKEN: "npm-secret-token", ...oidc })), /in bogusyogi\/rightkit only/);
  await assert.rejects(run(sealed, ok, actionsEnv({ PUBLISH_UNITS: "@rightkit/a @rightkit/z", NODE_AUTH_TOKEN: "npm-secret-token", ...oidc })), /differ from the planned units/);
});

// ---- the reviewer-approved artifact list (ci/approved-artifacts.json) --------------------------------

test("upload refuses a sealed unit the reviewers did not approve, in both modes, before any client, request or token is used", async () => {
  const sealed = sealNpm({ units: [["@rightkit/a", "packages/a"], ["@rightkit/b", "packages/b"]] });
  const receiptsDir = receipts(sealed.dir, sealed.manifest);
  const [a, b] = sealed.manifest.units;
  const entry = (unit, over = {}) => approvedEntry("npm", unit, { reviewRef: "pulse:64ffc522", ...over });
  const write = (doc) => { const file = path.join(tmp("approved"), "approved-artifacts.json"); fs.writeFileSync(file, typeof doc === "string" ? doc : JSON.stringify(doc)); return file; };
  const list = (...artifacts) => ({ schema: 1, artifacts });
  const touched = [];
  const spawn = spy({ npm: () => { touched.push("npm"); return { status: 0, stdout: "", stderr: "" }; } }).spawn;
  const fetchImpl = async () => { touched.push("fetch"); throw new Error("no network"); };
  const dryEnv = actionsEnv({ PUBLISH_UNITS: "@rightkit/a @rightkit/b" });
  const realEnv = { ...dryEnv, NODE_AUTH_TOKEN: "npm-secret-token", ...oidc };
  const attempt = (approvedFile, dryRun) => executeUploadRaw({ env: dryRun ? dryEnv : realEnv, packDir: sealed.dir, receiptsDir, approvedFile, outDir: tmp("out"), dryRun, spawn, fetchImpl, pause: 1 });
  const cases = [
    [undefined, /no approved-artifacts file was provided/],
    [path.join(tmp("none"), "approved-artifacts.json"), /is missing; no artifact is reviewer-approved/],
    [write("{ not json"), /is not JSON/],
    [write("[]"), /is not an object/],
    [write(list()), /@rightkit\/a@0\.1\.0: not reviewer-approved/],
    [write(list(entry(a))), /@rightkit\/b@0\.1\.0: not reviewer-approved/],
    [write(list(entry(b))), /@rightkit\/a@0\.1\.0: not reviewer-approved/],
    [write(list(entry(a), entry(b, { sha256: "e".repeat(64) }))), /@rightkit\/b@0\.1\.0: sealed sha256 [0-9a-f]{12} is not the reviewer-approved eeeeeeeeeeee \(pulse:64ffc522\)/],
    [write(list(entry(a), entry(b, { version: "0.1.1" }))), /@rightkit\/b@0\.1\.0: not reviewer-approved/],
    [write(list(entry(a), entry(b, { name: "@rightkit/bb" }))), /@rightkit\/b@0\.1\.0: not reviewer-approved/],
    [write(list(entry(a), entry(b, { registry: "crates" }))), /@rightkit\/b@0\.1\.0: not reviewer-approved/],
    // malformed files fail closed even when the sealed units are listed
    [write({ schema: 3, artifacts: [entry(a), entry(b)] }), /schema 3 is not 1 or 2/],
    [write({ schema: 1, artifacts: [entry(a), entry(b)], extra: true }), /exactly the keys artifacts, schema/],
    [write({ schema: 1 }), /artifacts is not a list/],
    [write(list(entry(a), entry(b, { reviewer: "" }))), /reviewer must be a non-empty string/],
    [write(list(entry(a), entry(b, { reviewRef: undefined }))), /exactly the keys name, registry, reviewRef, reviewer, sha256, version/],
    [write(list(entry(a), entry(b, { sha256: b.sha256.toUpperCase() }))), /sha256 must be 64 lowercase hex/],
    [write(list(entry(a), entry(b, { version: "^0.1.0" }))), /version must be a plain MAJOR\.MINOR\.PATCH/],
    [write(list(entry(a), entry(b), entry(b))), /is approved twice/],
    [write(list(entry(a), entry(b), { ...entry(b), extra: 1 })), /exactly the keys/],
  ];
  for (const [approvedFile, pattern] of cases) {
    for (const dryRun of [true, false]) await assert.rejects(attempt(approvedFile, dryRun), pattern, `${dryRun ? "dry run" : "real"}: ${pattern}`);
  }
  assert.deepEqual(touched, [], "a refused approval never reaches a registry client or request");
  // exact approval passes; approvals for other units or versions never widen or break it
  const exact = write(list(entry(a), entry(b)));
  await attempt(exact, true);
  await attempt(write(list(entry(a, { version: "0.0.9" }), entry(a), entry(b), { ...entry(a), name: "@rightkit/other" })), true);
  assert.deepEqual(touched, [], "a dry run sends nothing");
  // the default path is the committed file next to the uploader: the real job cannot be pointed elsewhere
  assert.equal(path.relative(path.resolve(fileURLToPath(new URL("../..", import.meta.url))), APPROVED_ARTIFACTS_FILE).split(path.sep).join("/"), "ci/approved-artifacts.json");
  await assert.rejects(executeUploadRaw({ env: dryEnv, packDir: sealed.dir, receiptsDir, outDir: tmp("out"), dryRun: true, spawn, fetchImpl }), /no approved-artifacts file was provided/, "omitting approvedFile refuses");
});

test("upload (crates) refuses an unapproved or re-hashed crate before any PUT", async () => {
  const sealed = sealCrates(["rightkit-a", "rightkit-b"]);
  const receiptsDir = receipts(sealed.dir, sealed.manifest);
  const [a, b] = sealed.manifest.units;
  const write = (artifacts) => { const file = path.join(tmp("approved"), "approved-artifacts.json"); fs.writeFileSync(file, JSON.stringify({ schema: 1, artifacts })); return file; };
  const requests = [];
  const fetchImpl = async (url, options = {}) => { requests.push(`${options.method ?? "GET"} ${url}`); throw new Error("no network"); };
  const env = actionsEnv({ PUBLISH_REGISTRY: "crates", PUBLISH_UNITS: "rightkit-a rightkit-b", CARGO_REGISTRY_TOKEN: "crate-secret-token" });
  const attempt = (approvedFile) => executeUploadRaw({ env, packDir: sealed.dir, receiptsDir, approvedFile, outDir: tmp("out"), dryRun: false, spawn: spy().spawn, fetchImpl, pause: 1 });
  await assert.rejects(attempt(write([approvedEntry("crates", a)])), /rightkit-b@0\.1\.0: not reviewer-approved/);
  await assert.rejects(attempt(write([approvedEntry("crates", a), approvedEntry("crates", b, { sha256: "d".repeat(64) })])), /rightkit-b@0\.1\.0: sealed sha256 .* is not the reviewer-approved dddddddddddd/);
  await assert.rejects(attempt(write([approvedEntry("crates", a), approvedEntry("npm", b)])), /rightkit-b@0\.1\.0: not reviewer-approved/);
  assert.deepEqual(requests, [], "nothing was fetched or PUT");
});

test("ci/approved-artifacts.json is well formed and holds exactly the reviewer-signed artifacts", () => {
  const { entries, problems } = loadApprovedArtifacts(APPROVED_ARTIFACTS_FILE);
  assert.deepEqual(problems, []);
  assert.deepEqual(entries.map((e) => `${e.registry} ${e.name}@${e.version} ${e.sha256}`).sort(), [
    "crates rightkit-child-process@0.5.4 b0e29813abd52bdba7629f4a82fa506338c21163d7d8675ce37fd63ea79a5105",
    "crates rightkit-crash@0.3.4 e59139ab374886e6c84b5fa8de86119d6cdfbd0d353baf2ffa82e756c93e796c",
    "crates rightkit-crash@0.3.5 50a29ac424871a0d65fd186c6f65f0ebadc11fdbda9270e1446ce490c06dc0d0",
    "crates rightkit-credentials@0.4.0 c74c8f18566064482803651b63d03571ac75343f673ca8c10580f4c7e5ca936f",
    "crates rightkit-credentials@0.4.1 eff9ae836419d4a38ea1454d1374ed49846e647b6cc1f1780d297c185fe8da58",
    "crates rightkit-credentials@0.4.2 4d6eae05bf2b22bde1bf2b6c0ddbdeb072f12a07bfe3a3037c754f4ef95c22e7",
    "crates rightkit-db@0.5.1 5faf1c33643ce547f0ed1b73a2bf975390095cbd99476f737a2e35417afca4fc",
    "crates rightkit-fs@0.4.6 8e901f915725e211a7acb5b4dfc99799adc03c3682ddb89a10e996e9d816efea",
    "crates rightkit-http@0.3.8 7764a1e201a6ea18222ff936e4ee36024c08fa79c0852bbf73eed6d94c2a4837",
    "crates rightkit-license@0.2.4 290cd2b85efffa9e7651b57cdfc070c5ee0d12baa27009235cdf4b5acca44386",
    "crates rightkit-logs@0.3.4 c37d3ed6ae2d2c3199767cd07c152706a3bc8242647f3f30c3290257535f2855",
    "crates rightkit-logs@0.3.5 3d00b6f923404ec52f3c42b6c1fc17da351d92a8c479208540716ef00ee4af19",
    "crates rightkit-oauth@0.10.0 724aa2b0f39d30efe8d0779059e62d8763e160e836b067cae324795a7a0ea426",
    "crates rightkit-overlay@0.2.0 d80bdc9880c338f6ddf08a3c7168865ae1499977f238f5f06f51c8ffa506b990",
    "crates rightkit-pdf@0.3.3 b099552e03cffb91f19c0148e70c8bd27a3a9ae17f0d3b413170e2188f2ea1b8",
    "crates rightkit-playback@0.3.0 3630a32d8ba9396c9d523713572afce43312e33b504173080fbac37a463bf547",
    "npm @rightkit/app-update@0.4.1 422f7281f7beba0453ac1250b823f9cb9e65cb290c7a098372c3888641813a09",
    "npm @rightkit/doc-render@0.2.1 05d226219b9f7d783d6a106dd1cf0fd6a50b238715c348ebc6c3b4bebbe18e43",
    "npm @rightkit/license@0.3.0 9cccfca32a85198f6bb1f863c8e16fe16260e2c5e6ee99d9d5ee169b87302082",
    "npm @rightkit/logs@0.1.6 72bb07be21cd4138458a635d9c48b7619f3b2874c0758089972e40015de0286c",
    "npm @rightkit/onboarding@0.6.3 9db84e3f619ab01cc932e5a9323ee77022287c82e752604a339fc3ddff8047a3",
    "npm @rightkit/qa-chrome@0.1.10 60e7bee4a1b92d409bf67decac5e60de208eede8c16f846e8e3272f11f964797",
    "npm @rightkit/qa-chrome@0.1.11 699a328926cbee90eaba0fbceab17ccea821f05d47ec0a023a66c60fa3c6ebb2",
    "npm @rightkit/qa@0.5.2 c55badace046a991171fa48e5e070b5822c5ba0f429fd8e5b114e490bd70031d",
    "npm @rightkit/react-shell@0.7.2 b0d6f495eaccfafaa5191d9c502c07a11cae84b58572597a10b69055103a8809",
    "npm @rightkit/release@0.4.1 725f265b2df780bd066a3749a75c7ce2a34e947a63e2d78dbd79df69045c9eec",
    "npm @rightkit/settings-ui@0.7.0 723b019f8f27f0d6c6600d846098040d21e28a17d0fb1cdf4e9b577f18000475",
    "npm @rightkit/tauri-shell@0.1.2 dd71d57ac0250c75a27871d5d6ae5231c05ec8033934ddf027be032ee5089952",
    "npm @rightkit/tauri@0.3.0 4441a7191e5da878dcd6029206cc0fafcc746de755282f6553183668eeb7a0b2",
  ]);
  // Each approval names its exact reviewer: the first three compared exact CI archives with previously signed
  // Mac tar streams; later ones are R8 exact-archive SIGNs on a single-unit qualification run.
  const foundation = { reviewer: "Oracle (agent source-byte review)", reviewRef: "agent-review:/root/npm_foundation_review/39398432518/source-bytes" };
  const reviews = {
    "rightkit-crash@0.3.5": { reviewer: "Oracle (exact archive and native receipt review)", reviewRef: "agent-review:/root/crash035_final_review/39535294878/crash-0.3.5" },
    "@rightkit/app-update@0.4.1": { reviewer: "Oracle (exact archive and native receipt review)", reviewRef: "agent-review:/oracle/app-update041/39579320760" },
    "rightkit-playback@0.3.0": { reviewer: "Oracle (exact archive and native receipt review)", reviewRef: "agent-review:/oracle/playback030/39583020425" },
    "@rightkit/settings-ui@0.7.0": { reviewer: "Oracle (exact archive and native receipt review)", reviewRef: "agent-review:/oracle/settings-ui070/39588514846" },
    "@rightkit/release@0.4.1": { reviewer: "Oracle (exact archive and native receipt review)", reviewRef: "agent-review:/oracle/release041/39592147750" },
    "rightkit-credentials@0.4.0": { reviewer: "Oracle (exact archive and native receipt review)", reviewRef: "agent-review:/oracle/credentials040/39610069491" },
    "rightkit-credentials@0.4.1": { reviewer: "Astra (gpt-6-astra high, adversarial review)", reviewRef: "https://github.com/bogusyogi/rightkit/issues/880#issuecomment-6321286407" },
    "rightkit-overlay@0.2.0": { reviewer: "Oracle (exact archive and native receipt review)", reviewRef: "agent-review:/root/overlay020_final_review/39531709031/overlay-0.2.0" },
    "rightkit-pdf@0.3.3": { reviewer: "Oracle (exact archive and native receipt review)", reviewRef: "agent-review:/root/pdf033_final_review/39531711587/pdf-0.3.3" },
    "@rightkit/qa@0.5.2": {"reviewer":"Oracle (exact archive and native receipt review)","reviewRef":"agent-review:/root/review_chrome_diagnostic/39516076778/qa-0.5.2"},
    "rightkit-crash@0.3.4": {"reviewer":"Oracle (exact archive and native receipt review)","reviewRef":"agent-review:/root/runtime_closure_review/39512771341/crash-0.3.4"},
    "rightkit-db@0.5.1": { reviewer: "Oracle (exact archive and native receipt review)", reviewRef: "agent-review:/root/db_candidate_review/39498324762/db-0.5.1" },
    "@rightkit/tauri@0.3.0": { reviewer: "Oracle (exact archive and native receipt review)", reviewRef: "agent-review:/root/hb_post_foundation/39505901290/tauri-0.3.0" },
    "rightkit-logs@0.3.4": { reviewer: "Oracle (exact archive and native receipt review)", reviewRef: "agent-review:/root/hb_post_foundation/39495922938/logs-0.3.4-runtime-39500183977" },
    "rightkit-logs@0.3.5": { reviewer: "Oracle (exact archive, native receipts and runtime proof review)", reviewRef: "agent-review:/root/runtime_closure_review/39509306414/logs-0.3.5-runtime-39510741371" },
    "@rightkit/tauri-shell@0.1.2": { reviewer: "Oracle (exact archive and native receipt review)", reviewRef: "agent-review:/root/hb_view_foundation/39500169311/tauri-shell-0.1.2" },
    "@rightkit/license@0.3.0": foundation,
    "@rightkit/logs@0.1.6": foundation,
    "@rightkit/react-shell@0.7.2": foundation,
    "@rightkit/onboarding@0.6.3": { reviewer: "Oracle (exact archive and native receipt review)", reviewRef: "agent-review:/root/hb_view_foundation/39494691758/onboarding-0.6.3" },
    "@rightkit/doc-render@0.2.1": { reviewer: "RK2 Astra (R8 exact-archive SIGN)", reviewRef: "agent-review:/root/swift_native_evidence/39468856523/doc-render" },
    "@rightkit/qa-chrome@0.1.10": { reviewer: "Oracle (exact archive and native receipt review)", reviewRef: "agent-review:/root/review_chrome_diagnostic/39510808790/qa-chrome-0.1.10" },
    "@rightkit/qa-chrome@0.1.11": { reviewer: "Oracle (exact archive, source diff and pin review)", reviewRef: "agent-review:/Volumes/D/scratch/rk-qa-closure-0.1.11/39903415267/qa-chrome-0.1.11" },
    "rightkit-fs@0.4.6": { reviewer: "Oracle (exact archive and native receipt review)", reviewRef: "agent-review:/root/hb_post_foundation/39482266477/fs-0.4.6" },
    "rightkit-http@0.3.8": { reviewer: "Oracle (exact archive and native receipt review)", reviewRef: "agent-review:/root/hb_post_foundation/39485850149/http-0.3.8" },
    "rightkit-child-process@0.5.4": { reviewer: "Oracle (exact archive and native receipt review)", reviewRef: "agent-review:/root/hb_post_foundation/39486639316/child-process-0.5.4" },
    "rightkit-license@0.2.4": { reviewer: "Astra (gpt-6-astra high, adversarial review)", reviewRef: "https://github.com/bogusyogi/rightkit/issues/880#issuecomment-6326550593" },
    "rightkit-oauth@0.10.0": { reviewer: "Astra (gpt-6-astra high, adversarial review)", reviewRef: "https://github.com/bogusyogi/rightkit/issues/880#issuecomment-6326550593" },
    "rightkit-credentials@0.4.2": { reviewer: "Astra (gpt-6-astra high, adversarial review)", reviewRef: "https://github.com/bogusyogi/rightkit/issues/886#issuecomment-6328792371" },
  };
  assert.deepEqual(Object.fromEntries(entries.map((e) => [`${e.name}@${e.version}`, { reviewer: e.reviewer, reviewRef: e.reviewRef }])), reviews);
});

test("the uploader keeps its reviewer-approval gate, and every weakening of it is rejected by the contract", () => {
  const text = fs.readFileSync(new URL("./upload.mjs", import.meta.url), "utf8");
  assert.equal(assertUploadScript(text), true);
  assert.deepEqual(proveUploadScriptMutations(text), []);
  const names = UPLOAD_SCRIPT_MUTATIONS.map(([name]) => name);
  for (const required of ["the approval check is removed", "the approval check only runs for a real upload", "the approval check runs after the refusal point", "a missing approvedFile defaults to nothing", "the approved path comes from the environment", "the approved hash is no longer compared", "an unapproved unit is allowed"]) assert.ok(names.includes(required), required);
  assert.throws(() => assertUploadScript(text.replace("problems.push(...checkApprovedArtifacts({ manifest, registry, approvedFile }));", "")), /drops the reviewer-approval gate/);
});

test("upload re-checks provenance preconditions in the sealed tarball in both modes", async () => {
  const wrong = sealNpm({ packageJson: (name, dir) => npmPackageJson(name, dir, { repository: { type: "git", url: "git+https://github.com/someone-else/rightkit.git", directory: dir } }) });
  const missing = sealNpm({ packageJson: (name, dir) => { const j = npmPackageJson(name, dir); delete j.repository; return j; } });
  const workspaceDep = sealNpm({ packageJson: (name, dir) => npmPackageJson(name, dir, { dependencies: { "@rightkit/b": "workspace:*" } }) });
  const noEntry = sealNpm({ packageJson: (name, dir) => npmPackageJson(name, dir, { main: "dist/missing.js" }) });
  for (const [sealed, pattern] of [[wrong, /must name https:\/\/github\.com\/bogusyogi\/rightkit/], [missing, /repository must be an object/], [workspaceDep, /workspace:/], [noEntry, /dist\/missing\.js, which is not in the tarball/]]) {
    for (const dryRun of [true, false]) {
      const env = dryRun ? actionsEnv() : actionsEnv({ NODE_AUTH_TOKEN: "npm-secret-token", ...oidc });
      await assert.rejects(executeUpload({ env, packDir: sealed.dir, receiptsDir: receipts(sealed.dir, sealed.manifest), outDir: tmp("out"), dryRun, spawn: spy().spawn, fetchImpl: async () => { throw new Error("no network"); } }), pattern);
    }
  }
});

test("upload (crates) PUTs the sealed bytes to the publish API with the token on that request only", async () => {
  const sealed = sealCrates(["rightkit-a", "rightkit-b"]);
  const receiptsDir = receipts(sealed.dir, sealed.manifest);
  const requests = [];
  // A fresh registry view per upload: the first index read of a crate (upload.mjs's reconciliation) finds nothing; later reads find it.
  const makeFetch = () => {
    const asked = new Set();
    return async (url, options = {}) => {
      requests.push({ url: String(url), options });
      if (String(url) === CRATES_PUBLISH_URL) return { status: 200, ok: true, text: async () => JSON.stringify({ warnings: {} }) };
      const name = String(url).split("/").pop();
      const unit = sealed.manifest.units.find((u) => u.name === name);
      if (!asked.has(name)) { asked.add(name); return { status: 404, ok: false, text: async () => "" }; }
      return { status: 200, ok: true, text: async () => JSON.stringify({ vers: "0.1.0", cksum: unit.sha256 }) };
    };
  };
  const fetchImpl = makeFetch();
  const { spawn, calls } = spy();
  const env = actionsEnv({ PUBLISH_REGISTRY: "crates", PUBLISH_UNITS: "rightkit-a rightkit-b", CARGO_REGISTRY_TOKEN: "crate-secret-token" });
  const doc = await executeUpload({ env, packDir: sealed.dir, receiptsDir, outDir: tmp("out"), dryRun: false, spawn, fetchImpl, pause: 1 });
  const puts = requests.filter((r) => r.options.method === "PUT");
  assert.deepEqual(puts.map((r) => r.url), [CRATES_PUBLISH_URL, CRATES_PUBLISH_URL]);
  assert.equal(CRATES_PUBLISH_URL, "https://crates.io/api/v1/crates/new");
  for (const [i, unit] of sealed.manifest.units.entries()) {
    const expected = buildCratePublishBody(fs.readFileSync(path.join(sealed.dir, unit.metaFile), "utf8"), fs.readFileSync(path.join(sealed.dir, unit.file)));
    assert.ok(puts[i].options.body.equals(expected), "the body is the length-prefixed metadata and the sealed crate bytes");
    assert.equal(puts[i].options.headers.authorization, "crate-secret-token");
    assert.equal(puts[i].options.redirect, "error");
  }
  assert.ok(requests.filter((r) => r.options.method !== "PUT").every((r) => !JSON.stringify(r.options.headers).includes("crate-secret-token")), "no other request carries the token");
  assert.ok(calls.every((c) => c.command === "tar"), "no cargo, no compiler, no install: only tar for the integrity listing");
  assert.deepEqual(doc.receipts.map((r) => [r.unit, r.published, r.registryVerified]), [["rightkit-a", true, true], ["rightkit-b", true, true]]);
  assert.equal(puts[0].options.headers.accept, "application/json", "the documented Accept header is sent");
  // HTTP 200 with an errors array is a failure, recorded as not published; warnings are surfaced; a non-JSON body is refused
  const answering = (body, status = 200, inner = null) => async (url, options = {}) => (String(url) === CRATES_PUBLISH_URL ? { status, ok: status < 400, text: async () => body } : (inner ??= makeFetch())(url, options));
  const published = [];
  const one = { env: actionsEnv({ PUBLISH_REGISTRY: "crates", PUBLISH_UNITS: "rightkit-a rightkit-b", CARGO_REGISTRY_TOKEN: "crate-secret-token" }), packDir: sealed.dir, receiptsDir, dryRun: false, spawn, pause: 1 };
  await assert.rejects(executeUpload({ ...one, outDir: tmp("out"), fetchImpl: answering(JSON.stringify({ errors: [{ detail: "crate version `0.1.0` is already uploaded crate-secret-token" }] })) }), (error) => /with errors for rightkit-a@0\.1\.0; not recorded as published: crate version `0\.1\.0` is already uploaded/.test(error.message) && !error.message.includes("crate-secret-token"));
  const outErr = tmp("out");
  await assert.rejects(executeUpload({ ...one, outDir: outErr, fetchImpl: answering(JSON.stringify({ errors: [{ detail: "nope" }] })) }), /with errors/);
  const errRows = JSON.parse(fs.readFileSync(path.join(outErr, "publication-receipt.json"), "utf8")).receipts;
  assert.deepEqual(errRows.map((r) => [r.unit, r.published, r.registryVerified, r.outcome]), [["rightkit-a", false, false, "failed"]], "an errors body records a failed attempt, never a publication");
  await assert.rejects(executeUpload({ ...one, outDir: tmp("out"), fetchImpl: answering("<html>ok</html>") }), /not the documented JSON/);
  const warned = await executeUpload({ ...one, outDir: tmp("out"), log: (l) => published.push(l), fetchImpl: answering(JSON.stringify({ warnings: { invalid_categories: ["bogus"], invalid_badges: [], other: ["readme is empty"] } })) });
  assert.match(published.join("\n"), /warnings for rightkit-a@0\.1\.0: invalid_categories: bogus; other: readme is empty/);
  assert.deepEqual(warned.receipts[0].warnings, ["invalid_categories: bogus", "other: readme is empty"]);
  assert.deepEqual(parseCratesResponse('{"warnings":{"invalid_categories":[],"invalid_badges":[],"other":[]}}'), { errors: [], warnings: [], unreadable: false });
  assert.deepEqual(parseCratesResponse('{"errors":["plain"]}').errors, ["plain"]);
  assert.equal(parseCratesResponse("[]").unreadable, true);
  // a refusal carries the status and never the token; a dry run sends nothing
  const refused = answering("forbidden crate-secret-token", 403);
  await assert.rejects(executeUpload({ env, packDir: sealed.dir, receiptsDir, outDir: tmp("out"), dryRun: false, spawn, fetchImpl: refused }), (error) => /HTTP 403/.test(error.message) && !error.message.includes("crate-secret-token"));
  const dry = await executeUpload({ env: actionsEnv({ PUBLISH_REGISTRY: "crates", PUBLISH_UNITS: "rightkit-a rightkit-b" }), packDir: sealed.dir, receiptsDir, outDir: tmp("out"), dryRun: true, spawn, fetchImpl: async () => { throw new Error("no network"); }, log: () => {} });
  assert.equal(dry.receipts.length, 2);
  await assert.rejects(executeUpload({ env: actionsEnv({ PUBLISH_REGISTRY: "crates", PUBLISH_UNITS: "rightkit-a rightkit-b", NODE_AUTH_TOKEN: "npm-secret-token", CARGO_REGISTRY_TOKEN: "crate-secret-token" }), packDir: sealed.dir, receiptsDir, outDir: tmp("out"), dryRun: false, spawn, fetchImpl }), /NODE_AUTH_TOKEN must not be present/);
});

// ---- a retry of failed jobs: partial publication is reconciled, never erased, never republished --

test("upload retry: the second unit failed, the retry starts from the earlier receipt and publishes only the rest", async () => {
  const sealed = sealNpm({ units: [["@rightkit/a", "packages/a"], ["@rightkit/b", "packages/b"]] });
  const receiptsDir = receipts(sealed.dir, sealed.manifest);
  const env1 = actionsEnv({ PUBLISH_UNITS: "@rightkit/a @rightkit/b", NODE_AUTH_TOKEN: "npm-secret-token", ...oidc });
  const out1 = tmp("out1");
  const first = fakeNpm(sealed, { failing: ["@rightkit/b"] });
  await assert.rejects(executeUpload({ env: env1, packDir: sealed.dir, receiptsDir, previousDir: tmp("none"), outDir: out1, dryRun: false, spawn: first.spawn, fetchImpl: first.fetchImpl, pause: 1, log: () => {} }), /npm publish failed for @rightkit\/b@0\.1\.0/);
  const after1 = JSON.parse(fs.readFileSync(path.join(out1, "publication-receipt.json"), "utf8"));
  assert.deepEqual(after1.receipts.map((r) => [r.unit, r.published, r.registryVerified, r.outcome, r.attempt]), [["@rightkit/a", true, true, "published", "1"], ["@rightkit/b", false, false, "failed", "1"]], "the failed attempt records the success AND the failure");
  // Attempt 2 of the same run: a is on the registry (held), b is not. The earlier receipt is the starting point.
  const out2 = tmp("out2");
  const second = fakeNpm(sealed, { present: Object.fromEntries([...first.held]) });
  const lines = [];
  const doc = await executeUpload({ env: { ...env1, GITHUB_RUN_ATTEMPT: "2" }, packDir: sealed.dir, receiptsDir, previousDir: out1, outDir: out2, dryRun: false, spawn: second.spawn, fetchImpl: second.fetchImpl, pause: 1, log: (l) => lines.push(l) });
  assert.deepEqual(second.publishes, ["@rightkit/b"], "only the unit not on the registry is published");
  assert.deepEqual(doc.receipts.map((r) => [r.unit, r.published, r.registryVerified, r.outcome, r.attempt]), [["@rightkit/a", true, true, "published-by-earlier-attempt", "2"], ["@rightkit/b", true, true, "published", "2"]]);
  assert.equal(doc.receipts[0].publishedBy, "earlier-attempt");
  assert.equal(doc.receipts[0].earlierAttempt, "1");
  assert.equal(doc.receipts[0].earlierOutcome, "published");
  assert.deepEqual(doc.priorAttempts, ["1"]);
  assert.ok(doc.history.some((r) => r.unit === "@rightkit/a" && r.attempt === "1" && r.outcome === "published"), "attempt 1's row is kept as history, not overwritten");
  assert.ok(doc.history.some((r) => r.unit === "@rightkit/b" && r.outcome === "failed"), "attempt 1's failure is kept");
  assert.match(lines.join("\n"), /@rightkit\/a@0\.1\.0 is already on npm with the sealed hash: published by an earlier attempt/);
  assert.deepEqual(JSON.parse(fs.readFileSync(path.join(out2, "publication-receipt.json"), "utf8")), doc, "the receipt on disk is the one returned");
  // The ledger accepts what the retried receipt says.
  const ledger = ledgerEntries(doc);
  assert.deepEqual(ledger.entries.map((e) => e.name), ["@rightkit/a", "@rightkit/b"]);
});

test("upload retry: a version already on the registry with different bytes is refused before anything is sent", async () => {
  const sealed = sealNpm({ units: [["@rightkit/a", "packages/a"], ["@rightkit/b", "packages/b"]] });
  const receiptsDir = receipts(sealed.dir, sealed.manifest);
  const env = actionsEnv({ PUBLISH_UNITS: "@rightkit/a @rightkit/b", NODE_AUTH_TOKEN: "npm-secret-token", ...oidc });
  const [a] = sealed.manifest.units;
  // b is first in nothing: a is absent, b has other bytes. Nothing may be published, not even a.
  const fake = fakeNpm(sealed, { present: { "@rightkit/b": { shasum: "0".repeat(40), integrity: "sha512-other" } } });
  const out = tmp("out");
  await assert.rejects(executeUpload({ env, packDir: sealed.dir, receiptsDir, previousDir: tmp("none"), outDir: out, dryRun: false, spawn: fake.spawn, fetchImpl: fake.fetchImpl, pause: 1, log: () => {} }), /npm already serves different bytes for @rightkit\/b@0\.1\.0/);
  assert.deepEqual(fake.publishes, [], "no publish ran");
  const rows = JSON.parse(fs.readFileSync(path.join(out, "publication-receipt.json"), "utf8")).receipts;
  assert.deepEqual(rows.map((r) => [r.unit, r.outcome]), [["@rightkit/b", "refused"]]);
  assert.equal(a.name, "@rightkit/a");
  // An unreadable registry answer is not permission to publish either.
  const down = { ...fake, fetchImpl: async () => ({ status: 503, ok: false, text: async () => "" }) };
  await assert.rejects(executeUpload({ env, packDir: sealed.dir, receiptsDir, previousDir: tmp("none"), outDir: tmp("out"), dryRun: false, spawn: down.spawn, fetchImpl: down.fetchImpl, pause: 1, log: () => {} }), /cannot reconcile @rightkit\/a@0\.1\.0 with the npm registry: HTTP 503/);
  assert.deepEqual(fake.publishes, []);
});

test("upload retry: refuses a previous receipt that is not this run's, or that says verified what the registry no longer serves", async () => {
  const sealed = sealNpm();
  const receiptsDir = receipts(sealed.dir, sealed.manifest);
  const env = { ...actionsEnv({ NODE_AUTH_TOKEN: "npm-secret-token", ...oidc }), GITHUB_RUN_ATTEMPT: "2" };
  const unit = sealed.manifest.units[0];
  const prior = (patch = {}, rows) => {
    const dir = tmp("prior");
    fs.writeFileSync(path.join(dir, "publication-receipt.json"), JSON.stringify({ schema: 1, registry: "npm", dryRun: false, commit: SHA, runId: "88", runAttempt: "1", priorAttempts: [], history: [], receipts: rows ?? [{ unit: unit.name, version: unit.version, published: true, registryVerified: true, outcome: "published", attempt: "1" }], ...patch }));
    return dir;
  };
  const go = (previousDir) => { const fake = fakeNpm(sealed); return executeUpload({ env, packDir: sealed.dir, receiptsDir, previousDir, outDir: tmp("out"), dryRun: false, spawn: fake.spawn, fetchImpl: fake.fetchImpl, pause: 1, log: () => {} }); };
  await assert.rejects(go(prior({ runId: "78" })), /previous attempt receipt refused: run 78 is not this run 88/);
  await assert.rejects(go(prior({ commit: "b".repeat(40) })), /previous attempt receipt refused: commit/);
  await assert.rejects(go(prior({ dryRun: true })), /not a real publication receipt/);
  await assert.rejects(go(prior({ registry: "crates" })), /registry crates is not npm/);
  await assert.rejects(go(prior({ runAttempt: "2" })), /attempt 2 is not earlier than this attempt 2/);
  await assert.rejects(go(prior({}, [{ unit: unit.name, version: unit.version, published: true, registryVerified: true, outcome: "published", attempt: "1" }])), /was recorded as published and verified by attempt 1, but the npm registry no longer serves it/);
  const garbage = tmp("prior"); fs.writeFileSync(path.join(garbage, "publication-receipt.json"), "not json");
  await assert.rejects(go(garbage), /previous attempt receipt is unreadable/);
  // No earlier receipt at all is a first attempt.
  const fresh = await go(tmp("none"));
  assert.equal(fresh.receipts[0].outcome, "published");
});

test("upload retry (crates): a crate already indexed with the sealed checksum is not PUT again; another checksum is refused", async () => {
  const sealed = sealCrates(["rightkit-a", "rightkit-b"]);
  const receiptsDir = receipts(sealed.dir, sealed.manifest);
  const env = actionsEnv({ PUBLISH_REGISTRY: "crates", PUBLISH_UNITS: "rightkit-a rightkit-b", CARGO_REGISTRY_TOKEN: "crate-secret-token", GITHUB_RUN_ATTEMPT: "2" });
  const held = new Map([["rightkit-a", sealed.manifest.units[0].sha256]]);
  const puts = [];
  const fetchImpl = async (url, options = {}) => {
    if (String(url) === CRATES_PUBLISH_URL) { puts.push(options.body.length); held.set("rightkit-b", sealed.manifest.units[1].sha256); return { status: 200, ok: true, text: async () => JSON.stringify({ warnings: {} }) }; }
    const name = String(url).split("/").pop();
    return held.has(name) ? { status: 200, ok: true, text: async () => JSON.stringify({ vers: "0.1.0", cksum: held.get(name) }) } : { status: 404, ok: false, text: async () => "" };
  };
  const doc = await executeUpload({ env, packDir: sealed.dir, receiptsDir, previousDir: tmp("none"), outDir: tmp("out"), dryRun: false, spawn: spy().spawn, fetchImpl, pause: 1, log: () => {} });
  assert.equal(puts.length, 1, "only rightkit-b is sent");
  assert.deepEqual(doc.receipts.map((r) => [r.unit, r.outcome]), [["rightkit-a", "published-by-earlier-attempt"], ["rightkit-b", "published"]]);
  held.set("rightkit-a", "f".repeat(64)); puts.length = 0;
  await assert.rejects(executeUpload({ env, packDir: sealed.dir, receiptsDir, previousDir: tmp("none"), outDir: tmp("out"), dryRun: false, spawn: spy().spawn, fetchImpl, pause: 1, log: () => {} }), /crates\.io already serves different bytes for rightkit-a@0\.1\.0/);
  assert.equal(puts.length, 0);
});

// ---- the ledger command is strict -------------------------------------------------------------

test("ledger records each verified publication, reads the ledger back, and fails on any incomplete recording", async () => {
  const root = tmp("ledger-root");
  const versionsFile = path.join(root, "packages", "release", "rightkit-versions.json");
  const copyFile = path.join(root, "packages", "build-control-policy", "src", "ownership-session-versions.json");
  fs.mkdirSync(path.dirname(versionsFile), { recursive: true }); fs.mkdirSync(path.dirname(copyFile), { recursive: true });
  fs.writeFileSync(versionsFile, "{}\n"); fs.writeFileSync(copyFile, "{}\n");
  const receiptFile = path.join(tmp("ledger-receipt"), "publication-receipt.json");
  const rows = [{ unit: "@rightkit/a", version: "0.1.0", published: true, registryVerified: true }, { unit: "@rightkit/b", version: "0.1.0", published: true, registryVerified: false }];
  fs.writeFileSync(receiptFile, JSON.stringify({ schema: 1, registry: "npm", dryRun: false, receipts: rows }));
  const compareVersions = (a, b) => (a === b ? 0 : a < b ? -1 : 1);
  const writer = ({ strictSeen = [], breakCopy = false, skipWrite = false, fail = false } = {}) => ({
    compareVersions,
    recordPublished: (entry, options) => {
      strictSeen.push(options.strict);
      if (fail) { if (options.strict) throw new Error("write failed"); return; }
      if (skipWrite) return;
      const doc = JSON.parse(fs.readFileSync(options.file, "utf8")); (doc.registry ??= {})[entry.registry] ??= {}; doc.registry[entry.registry][entry.name] = entry.version;
      const text = `${JSON.stringify(doc)}\n`; fs.writeFileSync(options.file, text); if (!breakCopy) fs.writeFileSync(copyFile, text);
    },
  });
  const seen = [];
  await commandLedger([receiptFile], { root, sync: writer({ strictSeen: seen }) });
  assert.deepEqual(seen, [true], "the unverified unit is skipped and the strict option is always on");
  assert.equal(JSON.parse(fs.readFileSync(versionsFile, "utf8")).registry.npm["@rightkit/a"], "0.1.0");
  await assert.rejects(commandLedger([receiptFile], { root, sync: writer({ fail: true }) }), /write failed/);
  fs.writeFileSync(versionsFile, "{}\n"); fs.writeFileSync(copyFile, "{}\n");
  await assert.rejects(commandLedger([receiptFile], { root, sync: writer({ skipWrite: true }) }), /ledger is incomplete[\s\S]*npm @rightkit\/a@0\.1\.0 is not in the ledger/);
  fs.writeFileSync(versionsFile, "{}\n");
  await assert.rejects(commandLedger([receiptFile], { root, sync: writer({ breakCopy: true }) }), /ownership-session-versions\.json differs from rightkit-versions\.json/);
  fs.writeFileSync(receiptFile, JSON.stringify({ schema: 1, registry: "npm", dryRun: false, receipts: [rows[1]] }));
  await assert.rejects(commandLedger([receiptFile], { root, sync: writer() }), /nothing to record/);
  fs.writeFileSync(receiptFile, JSON.stringify({ schema: 1, registry: "npm", dryRun: true, receipts: rows }));
  await assert.rejects(commandLedger([receiptFile], { root, sync: writer() }), /dry-run receipt is not a publication/);
  await assert.rejects(commandLedger([], { root, sync: writer() }), /usage/);
});

test("child environments are an allow-list, never process.env wholesale", () => {
  const source = { PATH: "/bin", HOME: "/h", GITHUB_SHA: "s", RUNNER_TEMP: "/t", ACTIONS_ID_TOKEN_REQUEST_URL: "u", NODE_AUTH_TOKEN: "npm-secret-token", CARGO_REGISTRY_TOKEN: "crate-secret-token", GITHUB_TOKEN: "g", AWS_SECRET_ACCESS_KEY: "k", INPUT_UNITS: "x", npm_config_registry: "evil" };
  assert.deepEqual(Object.keys(childEnv(source)).sort(), ["ACTIONS_ID_TOKEN_REQUEST_URL", "GITHUB_SHA", "HOME", "PATH", "RUNNER_TEMP"]);
  assert.equal(childEnv(source, { NODE_AUTH_TOKEN: "npm-secret-token" }).NODE_AUTH_TOKEN, "npm-secret-token");
});

test("the crates.io publish body is length-prefixed metadata then the crate", () => {
  const meta = JSON.stringify({ name: "x", vers: "0.1.0" }); const crateBytes = Buffer.from([1, 2, 3, 4, 5]);
  const body = buildCratePublishBody(meta, crateBytes);
  assert.equal(body.readUInt32LE(0), Buffer.byteLength(meta));
  assert.equal(body.subarray(4, 4 + Buffer.byteLength(meta)).toString(), meta);
  assert.equal(body.readUInt32LE(4 + Buffer.byteLength(meta)), 5);
  assert.ok(body.subarray(8 + Buffer.byteLength(meta)).equals(crateBytes));
});

test("crate upload metadata comes from the packaged manifest as Cargo's NewCrate shape", () => {
  const meta = buildCrateUploadMetadata({
    name: "rightkit-a", version: "0.1.0", authors: ["A"], description: "d", license: "MIT", repository: "https://github.com/bogusyogi/rightkit", readme: "README.md", keywords: ["k"], categories: [], rust_version: "1.80",
    features: { default: ["x"], x: [] },
    dependencies: [
      { name: "rightkit-b", req: "^0.2", kind: null, optional: false, uses_default_features: true, features: [], target: null, registry: null, rename: null },
      { name: "serde", req: "^1", kind: "dev", optional: true, uses_default_features: false, features: ["derive"], target: "cfg(unix)", registry: null, rename: "serde1" },
    ],
  }, "# readme\n");
  assert.equal(meta.name, "rightkit-a"); assert.equal(meta.vers, "0.1.0"); assert.equal(meta.readme, "# readme\n"); assert.equal(meta.readme_file, "README.md");
  assert.deepEqual(meta.deps[0], { optional: false, default_features: true, name: "rightkit-b", features: [], version_req: "^0.2", target: null, kind: "normal", registry: null, explicit_name_in_toml: null });
  assert.deepEqual(meta.deps[1], { optional: true, default_features: false, name: "serde", features: ["derive"], version_req: "^1", target: "cfg(unix)", kind: "dev", registry: null, explicit_name_in_toml: "serde1" });
});

// ---- pack and verify with intercepted child processes -----------------------------------------

function npmWorkspaceRoot(names = ["a"]) {
  const root = tmp("ws");
  for (const n of names) {
    fs.mkdirSync(path.join(root, "packages", n), { recursive: true });
    fs.writeFileSync(path.join(root, "packages", n, "package.json"), JSON.stringify({ ...npmPackageJson(`@rightkit/${n}`, `packages/${n}`), scripts: { test: "x" } }));
  }
  return root;
}

test("pack (npm) runs pnpm pack per unit with no credential, seals the bytes, and refuses a bad manifest", () => {
  const root = npmWorkspaceRoot(["a", "b"]);
  const outRoot = tmp("run");
  let badRepo = false;
  const pnpm = (args) => {
    const unit = args[1].split("/").at(-1);
    const dest = args[args.indexOf("--pack-destination") + 1];
    makeTgz(path.join(dest, `rightkit-${unit}-0.1.0.tgz`), "package", { "package.json": JSON.stringify(npmPackageJson(`@rightkit/${unit}`, `packages/${unit}`, badRepo ? { repository: { type: "git", url: "git+https://github.com/someone-else/rightkit.git", directory: `packages/${unit}` } } : {})), "dist/index.js": "x" });
    return { status: 0 };
  };
  const { spawn, calls } = spy({ pnpm, git: () => ({ status: 0, stdout: `${SHA}\n` }) });
  const env = actionsEnv({ RUNNER_TEMP: outRoot, PUBLISH_UNITS: "@rightkit/a @rightkit/b" });
  const manifest = commandPack({ env, spawn, root });
  const packs = calls.filter((c) => c.command === "pnpm");
  assert.deepEqual(packs.map((c) => c.args.slice(0, 3)), [["--dir", "packages/a", "pack"], ["--dir", "packages/b", "pack"]]);
  assert.deepEqual(manifest.units.map((u) => u.name), ["@rightkit/a", "@rightkit/b"]);
  assert.equal(manifest.commit, SHA);
  const onDisk = JSON.parse(fs.readFileSync(path.join(outRoot, "rk-pack", "manifest.json"), "utf8"));
  assert.equal(onDisk.units[0].sha256, sha256(fs.readFileSync(path.join(outRoot, "rk-pack", onDisk.units[0].file))));
  assert.throws(() => commandPack({ env: { ...env, NODE_AUTH_TOKEN: "npm-secret-token" }, spawn, root }), /pack and verify must not receive secrets/);
  assert.throws(() => commandPack({ env: { ...env, ...oidc }, spawn, root }), /must not request id-token/);
  assert.throws(() => commandPack({ env: { ...env, GITHUB_REF: "refs/heads/x" }, spawn, root }), /refs\/heads\/main only/);
  badRepo = true;
  assert.throws(() => commandPack({ env, spawn, root }), /must name https:\/\/github\.com\/bogusyogi\/rightkit/);
});

test("build (npm) builds only the planned units and their workspace dependency closure, never the whole workspace", () => {
  const root = npmWorkspaceRoot(["a", "b", "c"]);
  const { spawn, calls } = spy({ pnpm: () => ({ status: 0 }) });
  const env = actionsEnv({ PUBLISH_UNITS: "@rightkit/a @rightkit/c" });
  const args = commandBuild({ env, spawn, root });
  const builds = calls.filter((c) => c.command === "pnpm");
  assert.equal(builds.length, 1, "one pnpm build, scoped by filters");
  assert.deepEqual(builds[0].args, ["--workspace-concurrency=1", "--if-present", "--filter", "@rightkit/a...", "--filter", "@rightkit/c...", "run", "build"]);
  assert.deepEqual(args, builds[0].args);
  for (const wide of ["-r", "--recursive", "--workspace-root"]) assert.ok(!builds[0].args.includes(wide), `no ${wide}`);
  assert.ok(!builds[0].args.some((a) => a.includes("@rightkit/b")), "an unrelated package is not built");
  assert.throws(() => commandBuild({ env: { ...env, NODE_AUTH_TOKEN: "npm-secret-token" }, spawn, root }), /must not receive secrets/);
  assert.throws(() => commandBuild({ env: { ...env, PUBLISH_REGISTRY: "crates", PUBLISH_UNITS: "rightkit-a" }, spawn, root }), /build applies to npm runs/);
  assert.throws(() => commandBuild({ env: { ...env, PUBLISH_UNITS: "@rightkit/zzz" }, spawn, root }), /not a publishable npm unit/);
  assert.throws(() => commandBuild({ env: { ...env, GITHUB_ACTIONS: "false" }, spawn, root }), /GitHub Actions only/);
  assert.throws(() => commandBuild({ env, spawn: spy({ pnpm: () => ({ status: 1 }) }).spawn, root }), /build of the planned units and their dependencies failed/);
});

test("the build closure arguments filter each named package with its dependencies and refuse anything that is not a package name", () => {
  assert.deepEqual(pnpmBuildClosureArgs(["@rightkit/logs", "rightkit-plain", "@rightkit/logs"]), ["--workspace-concurrency=1", "--if-present", "--filter", "@rightkit/logs...", "--filter", "rightkit-plain...", "run", "build"]);
  assert.throws(() => pnpmBuildClosureArgs([]), /no packages to build/);
  assert.throws(() => pnpmBuildClosureArgs([undefined]), /unsafe package name/);
  for (const bad of ["--filter", "-r", "!@rightkit/a", "@rightkit/a...", "...@rightkit/a", "[origin/main]", "./packages/a", "a b", "@rightkit/a;x"]) assert.throws(() => pnpmBuildClosureArgs([bad]), /unsafe package name/, bad);
});

test("pack (crates) runs one cargo package, reads the packaged manifest, and seals metadata beside the crate", () => {
  const root = tmp("ws");
  const target = tmp("target");
  const outRoot = tmp("run");
  const wsMeta = { workspace_root: root, target_directory: target, workspace_members: ["a", "b"], packages: [
    metaPkg("rightkit-a", "0.1.0", [], { id: "a", manifest_path: `${root}/crates/rightkit-a/Cargo.toml` }),
    metaPkg("rightkit-b", "0.1.0", [metaDep("rightkit-a", "^0.1", { source: null })], { id: "b", manifest_path: `${root}/crates/rightkit-b/Cargo.toml` }),
  ] };
  const cargo = (args) => {
    if (args[0] === "metadata" && args.includes("--manifest-path")) {
      const manifestPath = args[args.indexOf("--manifest-path") + 1];
      const name = path.basename(path.dirname(manifestPath)).replace(/-0\.1\.0$/, "");
      const deps = name === "rightkit-b" ? [metaDep("rightkit-a", "^0.1")] : [];
      return { status: 0, stdout: JSON.stringify({ packages: [{ name, version: "0.1.0", dependencies: deps, features: {}, readme: "README.md", authors: [], keywords: [], categories: [], manifest_path: manifestPath }] }) };
    }
    if (args[0] === "metadata") return { status: 0, stdout: JSON.stringify(wsMeta) };
    if (args[0] === "package") {
      fs.mkdirSync(path.join(target, "package"), { recursive: true });
      for (const name of ["rightkit-a", "rightkit-b"]) makeTgz(path.join(target, "package", `${name}-0.1.0.crate`), `${name}-0.1.0`, { "Cargo.toml": "x", "README.md": `# ${name}\n`, "src/lib.rs": "" });
      return { status: 0 };
    }
  };
  const { spawn, calls } = spy({ cargo, git: () => ({ status: 0, stdout: `${SHA}\n` }) });
  const env = actionsEnv({ PUBLISH_REGISTRY: "crates", PUBLISH_UNITS: "rightkit-a rightkit-b", RUNNER_TEMP: outRoot });
  const manifest = commandPack({ env, spawn, root });
  const packageCalls = calls.filter((c) => c.command === "cargo" && c.args[0] === "package");
  assert.equal(packageCalls.length, 1);
  assert.deepEqual(packageCalls[0].args, ["package", "--locked", "-p", "rightkit-a", "-p", "rightkit-b"]);
  assert.equal(calls.filter((c) => c.command === "cargo" && c.args[0] === "publish").length, 0, "pack never publishes");
  const upload = JSON.parse(fs.readFileSync(path.join(outRoot, "rk-pack", manifest.units[1].metaFile), "utf8"));
  assert.equal(upload.name, "rightkit-b");
  assert.equal(upload.readme, "# rightkit-b\n");
  assert.deepEqual(upload.deps.map((d) => [d.name, d.version_req, d.kind]), [["rightkit-a", "^0.1", "normal"]]);
  assert.equal(manifest.units[1].metaSha256, sha256(fs.readFileSync(path.join(outRoot, "rk-pack", manifest.units[1].metaFile))));
  assert.throws(() => commandPack({ env: { ...env, CARGO_REGISTRY_TOKEN: "crate-secret-token" }, spawn, root }), /must not receive secrets/);
});

test("verify re-hashes the sealed bytes, requires gate evidence, and binds the receipt to the manifest", async () => {
  const sealed = sealNpm();
  const runTemp = tmp("verify");
  const evidenceDir = path.join(runTemp, "rightkit-ci");
  fs.mkdirSync(evidenceDir, { recursive: true });
  const writeEvidence = (patch = {}, status = "pass") => {
    const result = { unit: `npm:${sealed.manifest.units[0].dir}`, command: "pnpm --filter @rightkit/a run test", isTest: true, code: 0, timedOut: false, seconds: 1, executed: 3, failed: 0, ignored: 0, skipped: 0, ok: true };
    const evidence = { schema: 2, ok: true, host: "windows", registry: "npm", sha: SHA, dependentClosure: { selection: [], expected: [], results: [], ok: true, problems: [] }, units: [{ name: "@rightkit/a", status, executed: 3, steps: [result] }], ...patch };
    fs.writeFileSync(path.join(evidenceDir, "unit-evidence.json"), JSON.stringify(evidence));
    fs.writeFileSync(path.join(evidenceDir, "summary.json"), JSON.stringify({ host: evidence.host, sha: SHA, results: [result] }));
  };
  writeEvidence();
  const env = actionsEnv({ RUNNER_TEMP: runTemp, PUBLISH_PACK_DIR: sealed.dir, GITHUB_ACTOR: "fixture-actor", GITHUB_JOB: "verify" });
  let consumerProblems = [];
  let consumerCalls = 0;
  const scanArchive = (options) => scanArchiveForSecrets({ ...options, scannerOwner: { packageFileViolations: () => [], scanCrateForSecrets: (dir) => fs.readdirSync(dir).filter((file) => fs.readFileSync(path.join(dir, file), "utf8").includes("fixture-sensitive-value")) } });
  let seenIncomplete;
  const consume = { npm: ({ units, incomplete }) => { consumerCalls++; seenIncomplete = incomplete; return new Map(units.map((u) => [u.name, { problems: [...consumerProblems], report: { entries: 1, imported: [u.name] } }])); }, crates: () => { throw new Error("not used"); } };
  const run = (over = {}, host = "windows") => commandVerify({ env: { ...env, ...over }, spawn: spy({ git: () => ({ status: 0, stdout: `${SHA}\n` }) }).spawn, host, consume, scanArchive });
  // The source probe's INCOMPLETE set is required and handed to the consume step; without it, or malformed, nothing is verified.
  await assert.rejects(() => run({ RK_IMPORT_EFFECTS_REPORT: "" }), /RK_IMPORT_EFFECTS_REPORT is not set/);
  const badReport = path.join(runTemp, "bad-report.json"); fs.writeFileSync(badReport, JSON.stringify({ schema: "other", incomplete: [] }));
  await assert.rejects(() => run({ RK_IMPORT_EFFECTS_REPORT: badReport }), /is not rightkit\.import-effects\/1/);
  const reportWith = (name, patch) => { const f = path.join(runTemp, `${name}.json`); fs.writeFileSync(f, JSON.stringify({ ...REPORT_OK, ...patch })); return f; };
  await assert.rejects(() => run({ RK_IMPORT_EFFECTS_REPORT: reportWith("stale", { commit: "0".repeat(40) }) }), /written for commit 0{40}, not the dispatched/);
  await assert.rejects(() => run({ RK_IMPORT_EFFECTS_REPORT: reportWith("tree", { tree: "1".repeat(40) }) }), /written for tree 1{40}, not this checkout's/);
  await assert.rejects(() => run({ RK_IMPORT_EFFECTS_REPORT: reportWith("red", { findings: 2 }) }), /findings is 2; a red probe is not carried forward/);
  await assert.rejects(() => run({ RK_IMPORT_EFFECTS_REPORT: reportWith("dirty", { clean: false, dirty: 3 }) }), /not exactly its commit \(3 changed\/untracked/);
  await assert.rejects(() => run({ RK_IMPORT_EFFECTS_REPORT: reportWith("noprobe", { probe: "x" }) }), /no probe code digest/);
  const receipt = await run();
  assert.equal(seenIncomplete?.schema, "rightkit.import-effects/1");
  assert.deepEqual(receipt.importEffects, { commit: SHA, tree: SHA, probe: "a".repeat(64), node: "v26.8.1", checked: 0, incomplete: 0 }, "the receipt records the source closure the sealed bytes were verified against");
  assert.equal(receipt.host, "windows");
  assert.equal(receipt.manifestRunId, "77");
  assert.equal(receipt.manifestRunAttempt, "1");
  assert.equal(receipt.runAttempt, "1");
  assert.equal(receipt.units[0].consumer.verified, true);
  assert.equal(receipt.units[0].archiveScan.archiveSha256, sealed.manifest.units[0].sha256);
  assert.equal(receipt.sourceGate.summarySha256, sha256(fs.readFileSync(path.join(evidenceDir, "summary.json"))));
  assert.equal(receipt.sourceGate.unitEvidenceSha256, sha256(fs.readFileSync(path.join(evidenceDir, "unit-evidence.json"))));
  assert.equal(receipt.sourceGate.tree, SHA);
  assert.equal(receipt.sourceGate.dependentClosure.ok, true);
  assert.equal(receipt.units[0].publicationEvidence.runs[0].passed, 3);
  assert.equal(receipt.units[0].publicationEvidence.runs[0].host.arch, process.arch);
  assert.equal(receipt.units[0].publicationEvidence.runs[0].host.os, process.platform);
  const unsafe = sealNpm({ extraFiles: { "hidden/config": "fixture-sensitive-value" } });
  const beforeUnsafe = consumerCalls;
  await assert.rejects(() => run({ PUBLISH_PACK_DIR: unsafe.dir }), /archiveScan proof/);
  assert.equal(consumerCalls, beforeUnsafe, "archive scan refusal precedes consumer execution");
  consumerProblems = ["@rightkit/a: ./dist/index.js did not resolve or import: boom"];
  await assert.rejects(() => run(), /did not resolve or import: boom/);
  consumerProblems = [];
  await assert.rejects(() => run({ GITHUB_RUN_ID: "78" }), /manifest run 77 is not this run 78/);
  await assert.rejects(() => run({ GITHUB_RUN_ATTEMPT: "" }), /GITHUB_RUN_ATTEMPT is not set/);
  assert.equal(receipt.manifestSha256, sha256(fs.readFileSync(path.join(sealed.dir, "manifest.json"))));
  assert.deepEqual(receipt.units.map((u) => [u.name, u.status, u.bytesVerified]), [["@rightkit/a", "pass", true]]);
  assert.ok(fs.existsSync(path.join(runTemp, "verify-receipt", "verify-windows.json")));
  await assert.rejects(() => run({ NODE_AUTH_TOKEN: "npm-secret-token" }), /must not receive secrets/);
  await assert.rejects(() => run({}, "linux"), /no sealed unit requires Linux verification/);
  await assert.rejects(() => run({}, "freebsd"), /macOS, Windows or Linux, not freebsd/);
  writeEvidence({ host: "macos" });
  await assert.rejects(() => run(), /gate evidence is for npm on macos/);
  writeEvidence({ units: [] });
  await assert.rejects(() => run(), /no gate unit evidence exists/);
  writeEvidence({}, "fail");
  await assert.rejects(() => run(), /gate status "fail"/);
  writeEvidence();
  fs.appendFileSync(path.join(sealed.dir, sealed.manifest.units[0].file), "x");
  await assert.rejects(() => run(), /sha256 does not match the manifest/);
});

test("a sealed crate must equal the commit's source, byte for byte", () => {
  const dir = tmp("crate");
  const file = path.join(dir, "rightkit-a-0.1.0.crate");
  makeTgz(file, "rightkit-a-0.1.0", { "Cargo.toml": "generated", "Cargo.toml.orig": "[package]\nname=\"rightkit-a\"\n", "src/lib.rs": "pub fn a() {}\n", "README.md": "# a\n" });
  const unit = { name: "rightkit-a", version: "0.1.0", dir: "crates/rightkit-a" };
  const blobs = { "crates/rightkit-a/Cargo.toml": "[package]\nname=\"rightkit-a\"\n", "crates/rightkit-a/src/lib.rs": "pub fn a() {}\n", "README.md": "# a\n" };
  const gitFor = (overrides = {}) => (args) => {
    const key = args[2].slice(`${SHA}:`.length);
    const content = { ...blobs, ...overrides }[key];
    return content === undefined ? { status: 128, stdout: Buffer.alloc(0) } : { status: 0, stdout: Buffer.from(content) };
  };
  const check = (overrides) => crateSourceProblems({ file, unit, commit: SHA, scratchRoot: dir, spawn: spy({ git: gitFor(overrides) }).spawn });
  assert.deepEqual(check({}), []);
  assert.match(check({ "crates/rightkit-a/src/lib.rs": "pub fn evil() {}\n" }).join(), /src\/lib\.rs in the sealed crate does not match/);
  assert.match(check({ "crates/rightkit-a/Cargo.toml": "[package]\nname=\"other\"\n" }).join(), /Cargo\.toml\.orig/);
  assert.match(check({ "README.md": undefined }).join(), /README\.md/);
});

// ---- the workflow parser ----------------------------------------------------------------------

test("the workflow YAML subset parser reads structure and rejects what it cannot parse", () => {
  const doc = parseYaml(`# c
name: x
on:
  workflow_dispatch:
    inputs:
      list:
        options: [npm, "crates"]
jobs:
  a:
    needs: [b]
    if: \${{ x == 'y' }} # trailing
    steps:
      - uses: act/x@abc # note
        with:
          k: "v: w"
      - name: s
        run: |
          echo "# kept"
          if true; then
            exit 1
          fi
        env:
          E: \${{ secrets.T }}
      - run: single line
`);
  assert.deepEqual(Object.keys(doc.on), ["workflow_dispatch"]);
  assert.deepEqual(doc.on.workflow_dispatch.inputs.list.options, ["npm", "crates"]);
  assert.deepEqual(doc.jobs.a.needs, ["b"]);
  assert.equal(doc.jobs.a.if, "${{ x == 'y' }}");
  assert.equal(doc.jobs.a.steps[0].uses, "act/x@abc");
  assert.equal(doc.jobs.a.steps[0].with.k, "v: w");
  assert.equal(doc.jobs.a.steps[1].run, 'echo "# kept"\nif true; then\n  exit 1\nfi\n', "a literal block keeps one trailing newline");
  assert.equal(parseYaml("a: |-\n  x\n  y\n\nb: |\n  z\n\n\nc: d\n").a, "x\ny");
  assert.equal(parseYaml("b: |\n  z\n\n\nc: d\n").b, "z\n");
  assert.equal(doc.jobs.a.steps[1].env.E, "${{ secrets.T }}");
  assert.equal(doc.jobs.a.steps[2].run, "single line");
  for (const [bad, pattern] of [
    ["a:\n\tb: 1\n", /tab/], ["a: &x 1\n", /anchors/], ["a: 1\na: 2\n", /duplicate key/], ["a: one\n  two\n", /multi-line plain/],
    ["a: {b: 1}\n", /flow mappings/], ["a: [b, [c]]\n", /nested flow/], ["a: *x\n", /anchors/], ["a:\n  b: 1\n c: 2\n", /indent/],
    // the review's example: an indentation indicator makes YAML keep the newlines a naive parser folds away
    ["run: >1\n  node scripts/ci/upload.mjs\n  run\n", /block scalar header ">1"/],
    ["run: >\n  a\n  b\n", /block scalar header ">"/], ["run: >-\n  a\n", /block scalar header/], ["run: >+\n  a\n", /block scalar header/],
    ["run: |2\n   a\n", /block scalar header "\|2"/], ["run: |1-\n  a\n", /block scalar header/], ["run: |+\n  a\n", /block scalar header/], ["run: |-2\n  a\n", /block scalar header/],
    ["a:\n  - |\n    x\n", /block scalar header/], ["a:\n  - >\n    x\n", /block scalar header/],
    ["&x a: 1\n", /cannot parse/], ["a:\n  - &x b\n", /anchors/], ["a: !!str b\n", /anchors, aliases and tags/], ["<<: *x\n", /anchors/],
    ["a: [b,\n  c]\n", /multi-line plain|flow sequence must close/], ["a: [b\n", /flow sequence must close/], ["a: \"b\n  c\"\n", /multi-line plain|unterminated/],
    ["a:\t1\n", /tabs are not allowed/], ["a:\n  b: |\n    x\n\ty\n", /tabs are not allowed/],
    ["a:\n  b: 1\n  b: 2\n", /duplicate key "b"/], ["a: 1\n---\nb: 2\n", /cannot parse/], ["? a\n: b\n", /cannot parse/], ["a:\n  - - b\n", /indicator/],
    ["a: |\n  x\n  \n    \n  y\n", /whitespace-only line/],
  ]) assert.throws(() => parseYaml(bad), pattern, JSON.stringify(bad));
});

// ---- qualification & promotion workflow trust boundaries --------------------------------------
const realWorkflowSkip = !fs.existsSync(new URL("../../.github/workflows/publish.yml", import.meta.url)) && "no publish.yml";
const realDoc = () => parseYaml(fs.readFileSync(new URL("../../.github/workflows/publish.yml", import.meta.url), "utf8"));

test("qualification packs once, retains both native receipts & has no publication privilege or dispatch dry-run", { skip: realWorkflowSkip }, () => {
  const doc = realDoc();
  assert.equal(assertPublishDoc(doc), true);
  assert.deepEqual(Object.keys(doc.jobs), ["guard", "scans", "plan", "build-swap", "sign-swap", "build-credentials-helper", "sign-credentials-helper", "pack", "verify", "verify-linux", "qualified"]);
  assert.deepEqual([...doc.jobs.verify.strategy.matrix.os].sort(), ["windows-2025", "xcode-27"]);
  assert.equal(doc.on.workflow_dispatch.inputs.dry_run, undefined);
  assert.equal(doc.jobs.plan.outputs.dry_run, undefined);
  assert.equal(doc.jobs.upload, undefined);
  assert.equal(doc.jobs["seal-check"], undefined);
  assert.equal(doc.concurrency.group, PUBLISH_CONCURRENCY_GROUP);
  assert.equal(doc.concurrency["cancel-in-progress"], "false");
  assert.equal(doc.jobs.pack.steps.filter((s) => s.run?.trim() === "node scripts/ci/publish.mjs pack").length, 1);
  assert.equal(doc.jobs.verify.steps.find((s) => /provision-chrome\.mjs/.test(s.run ?? "")).env.PUBLISH_DRY_RUN, "false");
  for (const name of ["Cache the pinned Chrome for Testing", "Provision the pinned Chrome for Testing"]) assert.equal(doc.jobs.verify.steps.find((s) => s.name === name).if.trim(), CHROME_IF);
  const receipt = doc.jobs.verify.steps.find((s) => s.with?.name === "publish-receipt-verify-${{ matrix.os }}");
  assert.equal(receipt.with.path, "${{ runner.temp }}/verify-receipt");
  assert.equal(receipt.with["if-no-files-found"], "error");
  assert.equal(receipt.with["retention-days"], "90");
  assert.deepEqual(proveMutations(doc), []);
  const names = MUTATIONS.map(([name]) => name);
  for (const required of ["qualification regains upload", "qualification regains approval before pack", "qualification packs twice", "qualification regains dry-run dispatch", "verify loses native receipt artifact", "drop the Windows verify host", "drop the macOS verify host", "qualification final gate accepts skipped verify", "pack builds the whole workspace", "pack drops the scoped build", "pack builds after sealing"]) assert.ok(names.includes(required), required);
});

test("packing & native verification stop after failed, skipped or cancelled required jobs", { skip: realWorkflowSkip }, () => {
  const doc = realDoc();
  assert.equal(doc.jobs.pack.if.trim(), PACK_IF);
  assert.equal(doc.jobs.verify.if.trim(), VERIFY_IF);
  const evaluate = (condition, { results, count = "3", cancelled = false, registry = "npm", release = false, credentials = false }) => condition.replace(SWAP_OK_OR_SKIPPED, "swap-statuses").replace(CREDENTIALS_OK_OR_SKIPPED, "credentials-statuses").replace(/^\$\{\{\s*|\s*\}\}$/g, "").split(" && ").every((term) => {
    if (term === "!cancelled()") return !cancelled;
    if (term === "swap-statuses") return registry === "npm" && release ? results["build-swap"] === "success" && results["sign-swap"] === "success" : results["build-swap"] === "skipped" && results["sign-swap"] === "skipped";
    if (term === "credentials-statuses") return registry === "crates" && credentials ? results["build-credentials-helper"] === "success" && results["sign-credentials-helper"] === "success" : results["build-credentials-helper"] === "skipped" && results["sign-credentials-helper"] === "skipped";
    const m = /^needs\.([a-z-]+)\.result == 'success'$/.exec(term);
    if (m) return results[m[1]] === "success";
    if (term === "needs.plan.outputs.count != '0'") return count !== "0";
    throw new Error(`unknown gate term: ${term}`);
  });
  const ok = { guard: "success", plan: "success", pack: "success", "build-swap": "skipped", "sign-swap": "skipped", "build-credentials-helper": "skipped", "sign-credentials-helper": "skipped" };
  for (const gate of [PACK_IF, VERIFY_IF]) {
    assert.equal(evaluate(gate, { results: ok }), true);
    assert.equal(evaluate(gate, { results: ok, count: "0" }), false);
    assert.equal(evaluate(gate, { results: ok, cancelled: true }), false);
    for (const upstream of ["guard", "plan"]) for (const bad of ["failure", "skipped", "cancelled"]) assert.equal(evaluate(gate, { results: { ...ok, [upstream]: bad } }), false);
  }
  for (const bad of ["failure", "skipped", "cancelled"]) assert.equal(evaluate(VERIFY_IF, { results: { ...ok, pack: bad } }), false);
  for (const bad of ["failure", "cancelled"]) assert.equal(evaluate(PACK_IF, { results: { ...ok, "sign-swap": bad } }), false);
  assert.equal(evaluate(PACK_IF, { results: ok, release: true }), false, "required signer cannot skip");
  assert.equal(evaluate(PACK_IF, { results: { ...ok, "build-swap": "success", "sign-swap": "success" }, release: true }), true);
  for (const bad of ["failure", "cancelled", "skipped"]) assert.equal(evaluate(PACK_IF, { results: { ...ok, "build-swap": bad, "sign-swap": "success" }, release: true }), false, "required unsigned build must succeed");
  for (const bad of ["failure", "cancelled"]) assert.equal(evaluate(VERIFY_IF, { results: { ...ok, "sign-credentials-helper": bad }, registry: "crates", credentials: true }), false);
  assert.equal(evaluate(VERIFY_IF, { results: ok, registry: "crates", credentials: true }), false, "required credentials signer cannot skip");
  assert.equal(evaluate(VERIFY_IF, { results: { ...ok, "build-credentials-helper": "success", "sign-credentials-helper": "success" }, registry: "crates", credentials: true }), true);
  for (const bad of ["failure", "cancelled", "skipped"]) assert.equal(evaluate(VERIFY_IF, { results: { ...ok, "build-credentials-helper": bad, "sign-credentials-helper": "success" }, registry: "crates", credentials: true }), false, "required unsigned credentials build must succeed");
  for (const [name, mutate] of MUTATIONS.filter(([n]) => /^(?:pack|verify) (?:condition (?:drops|uses)|accepts a (?:skipped|failed))/.test(n))) {
    const copy = structuredClone(doc); mutate(copy);
    assert.throws(() => assertPublishDoc(copy), /condition must be exactly/, name);
  }
});

test("native helper build is isolated from fresh source-free signer & same-run identity precedes signing secrets", { skip: realWorkflowSkip }, () => {
  const doc = realDoc(); const build = doc.jobs["build-swap"]; const signer = doc.jobs["sign-swap"];
  assert.equal(build.environment, undefined);
  assert.deepEqual(build.permissions, { contents: "read" });
  assert.equal(signer.if, SIGN_SWAP_IF);
  assert.deepEqual(signer.needs, ["guard", "plan", "build-swap"]);
  assert.deepEqual(signer.steps[0].with, { name: UNSIGNED_ARTIFACT, path: "${{ runner.temp }}/swap-unsigned" });
  assert.equal(signer.steps[1].run, UNSIGNED_VALIDATE_RUN);
  assert.deepEqual(signer.steps[1].env, { EXPECTED_SHA256: "${{ needs.build-swap.outputs.sha256 }}" });
  assert.match(signer.steps[1].run, /cmp "\$RUNNER_TEMP\/expected-swap-receipt.txt"/);
  assert.match(signer.steps[1].run, /shasum -a 256 -c rightkit-swap.sha256/);
  assert.equal(signer.steps.filter((step) => step.env?.APPLE_CERTIFICATE_BASE64).length, 1);
  assert.equal(signer.steps[2].env.APPLE_CERTIFICATE_BASE64, "${{ secrets.APPLE_CERTIFICATE_BASE64 }}");
  assert.equal(signer.steps.some((step) => /checkout|toolchain|setup-node|pnpm/.test(step.uses ?? "")), false);
  assert.equal(signer.steps.some((step) => /cargo |pnpm |npm |node /.test(step.run ?? "")), false);
  const names = MUTATIONS.map(([name]) => name);
  for (const name of ["build-swap joins signing environment", "build-swap requests OIDC", "signer reads secrets before validation", "signer checks out package code", "signer takes unsigned artifact from foreign run", "signer omits unsigned hash validation", "signer omits source run validation", "signer substitutes source attempt in receipt", "sign-swap rebuilds after receiving secrets", "pack admits skipped signer for required helper"]) assert.ok(names.includes(name), name);
});

test("verify stages the sealed signed-helper route for the macOS credentials consumer", async () => {
  const units = [{ name: "rightkit-credentials", version: "0.4.0" }];
  const staged = { RK_CONSUMER_HELPER_NAME: NATIVE_HELPER.name, RK_CONSUMER_HELPER_TEAM_ID: NATIVE_HELPER.teamId, RK_SOURCE_HELPER_APP: path.join(tmp("helper"), `${NATIVE_HELPER.name}.app`) };
  // out of scope: no staging, no requirement
  assert.deepEqual(stageSealedHelperEnv({ env: {}, registry: "crates", units, host: "windows" }), {});
  assert.deepEqual(stageSealedHelperEnv({ env: {}, registry: "crates", units: [{ name: "rightkit-license" }], host: "macos" }), {});
  assert.deepEqual(stageSealedHelperEnv({ env: staged, registry: "npm", units, host: "macos" }), {});
  assert.deepEqual(stageSealedHelperEnv({ env: staged, registry: "crates", units: ["rightkit-credentials"], host: "macos" }), staged);
  assert.deepEqual(stageSealedHelperEnv({ env: staged, registry: "crates", units, host: "macos" }), staged);
  // every malformed route refuses before any sealed byte is consumed
  for (const env of [{}, { ...staged, RK_CONSUMER_HELPER_NAME: "OtherHelper" }, { ...staged, RK_CONSUMER_HELPER_TEAM_ID: "OTHER" }, { ...staged, RK_CONSUMER_HELPER_NAME: undefined }]) {
    assert.throws(() => stageSealedHelperEnv({ env, registry: "crates", units, host: "macos" }), /signed helper name & team id/);
  }
  for (const source of [undefined, `relative/${NATIVE_HELPER.name}.app`, `/x/../${NATIVE_HELPER.name}.app`, "/x/OtherHelper.app"]) {
    assert.throws(() => stageSealedHelperEnv({ env: { ...staged, RK_SOURCE_HELPER_APP: source }, registry: "crates", units, host: "macos" }), /same-run signed helper app/);
  }
  // the sealed candidate reaches the consumer only with the staged route, and a missing route refuses first
  const sealed = sealCrates(["rightkit-credentials"]);
  const runTemp = tmp("verify-credentials");
  const evidenceDir = path.join(runTemp, "rightkit-ci");
  fs.mkdirSync(evidenceDir, { recursive: true });
  const result = { unit: "rust:rightkit-credentials", command: "cargo run --locked --offline -- native-helper", isTest: true, code: 0, timedOut: false, seconds: 1, executed: 1, failed: 0, ignored: 0, skipped: 0, ok: true };
  fs.writeFileSync(path.join(evidenceDir, "unit-evidence.json"), JSON.stringify({ schema: 2, ok: true, host: "macos", registry: "crates", sha: SHA, dependentClosure: { selection: [], expected: [], results: [], ok: true, problems: [] }, units: [{ name: "rightkit-credentials", status: "pass", executed: 1, steps: [result] }] }));
  fs.writeFileSync(path.join(evidenceDir, "summary.json"), JSON.stringify({ host: "macos", sha: SHA, results: [{ ...result, command: "cargo test -p rightkit-credentials --no-fail-fast" }] }));
  const blob = (name) => ({ "crates/rightkit-credentials/Cargo.toml": `[package]\nname = "rightkit-credentials"\n`, "crates/rightkit-credentials/src/lib.rs": "pub fn a() {}\n", "src/lib.rs": "pub fn a() {}\n" }[name]);
  const git = (args) => args[0] === "cat-file" ? { status: 0, stdout: Buffer.from(blob(args[2].slice(`${SHA}:`.length)) ?? "") } : { status: 0, stdout: `${SHA}\n` };
  const seen = [];
  const scanArchive = (options) => scanArchiveForSecrets({ ...options, scannerOwner: { packageFileViolations: () => [], scanCrateForSecrets: () => [] } });
  const consume = { npm: () => { throw new Error("not used"); }, crates: ({ env }) => { seen.push(env); return new Map([["rightkit-credentials", { problems: [], report: { entries: 1 } }]]); } };
  const base = actionsEnv({ RUNNER_TEMP: runTemp, PUBLISH_PACK_DIR: sealed.dir, GITHUB_ACTOR: "fixture-actor", GITHUB_JOB: "verify", PUBLISH_REGISTRY: "crates", PUBLISH_UNITS: "rightkit-credentials" });
  const run = (over = {}) => commandVerify({ env: { ...base, ...over }, spawn: spy({ git }).spawn, host: "macos", consume, scanArchive });
  await assert.rejects(() => run(), /signed helper name & team id/);
  await assert.rejects(() => run({ ...staged, RK_SOURCE_HELPER_APP: "/x/OtherHelper.app" }), /same-run signed helper app/);
  assert.equal(seen.length, 0, "a missing helper route refuses before the sealed consumer runs");
  const receipt = await run(staged);
  assert.equal(receipt.units[0].consumer.verified, true);
  assert.equal(seen.length, 1);
  assert.equal(seen[0].RK_SOURCE_HELPER_APP, staged.RK_SOURCE_HELPER_APP);
  assert.equal(seen[0].RK_CONSUMER_HELPER_NAME, NATIVE_HELPER.name);
  assert.equal(seen[0].RK_CONSUMER_HELPER_TEAM_ID, NATIVE_HELPER.teamId);
});

test("qualification aggregate always runs & refuses every missing prerequisite or empty plan", { skip: realWorkflowSkip }, () => {
  const doc = realDoc();
  const gate = doc.jobs.qualified;
  assert.equal(gate.if, "${{ always() }}");
  assert.deepEqual(gate.needs, ["guard", "plan", "pack", "verify", "verify-linux"]);
  assert.deepEqual(gate.steps[0].env, QUALIFIED_ENV);
  assert.equal(gate.steps[0].run, QUALIFIED_RUN);
  // Interpret only these shell test commands; this regression remains pure & host-independent.
  const accepts = (env) => gate.steps[0].run.trim().split("\n").every((line) => {
    const literal = /^test "\$([A-Z_]+)" (= success|-gt 0|-ge 0)$/.exec(line);
    if (literal) return literal[2] === "= success" ? env[literal[1]] === "success" : /^\d+$/.test(env[literal[1]] ?? "") && Number(env[literal[1]]) >= (literal[2] === "-gt 0" ? 1 : 0);
    const pair = /^test "\$([A-Z_]+)" = "\$([A-Z_]+)"$/.exec(line);
    assert.ok(pair, line);
    return env[pair[1]] !== undefined && env[pair[1]] === env[pair[2]];
  });
  // The workflow computes LINUX_EXPECTED_RESULT as `linux_count != '0' && 'success' || 'skipped'`.
  assert.equal(gate.steps[0].env.LINUX_EXPECTED_RESULT, "${{ needs.plan.outputs.linux_count != '0' && 'success' || 'skipped' }}");
  const env = ({ linux = "0", linuxResult = "skipped", ...over } = {}) => ({ GUARD_RESULT: "success", PLAN_RESULT: "success", PACK_RESULT: "success", VERIFY_RESULT: "success", UNIT_COUNT: "2", LINUX_COUNT: linux, VERIFY_LINUX_RESULT: linuxResult, LINUX_EXPECTED_RESULT: linux !== "0" ? "success" : "skipped", ...over });
  const success = env();
  assert.equal(accepts(success), true, "no Linux unit: verify-linux is skipped");
  for (const key of ["GUARD_RESULT", "PLAN_RESULT", "PACK_RESULT", "VERIFY_RESULT"]) for (const bad of ["failure", "skipped", "cancelled", ""]) assert.equal(accepts({ ...success, [key]: bad }), false, `${key}: ${bad}`);
  for (const count of ["0", "", "invalid"]) assert.equal(accepts({ ...success, UNIT_COUNT: count }), false);
  // A Linux-required publication needs a successful Linux verify; a skipped, failed or cancelled one never counts as a pass.
  assert.equal(accepts(env({ linux: "1", linuxResult: "success" })), true);
  for (const bad of ["skipped", "failure", "cancelled", ""]) assert.equal(accepts(env({ linux: "1", linuxResult: bad })), false, `required Linux ${bad}`);
  // No Linux unit: anything but a skipped verify-linux, or an unreadable count, is refused.
  for (const bad of ["success", "failure", "cancelled", ""]) assert.equal(accepts(env({ linuxResult: bad })), false, `unneeded Linux ${bad}`);
  for (const count of ["", "invalid", "-1"]) assert.equal(accepts(env({ linux: "1", linuxResult: "success", LINUX_COUNT: count })), false, `count ${count}`);
});

test("qualification contract finds every secret expression & exact-condition bypass", { skip: realWorkflowSkip }, () => {
  const broken = (edit) => { const doc = structuredClone(realDoc()); edit(doc); return () => assertPublishDoc(doc); };
  assert.throws(broken((d) => { d.jobs.pack.if += " || true"; }), /condition must be exactly/);
  assert.throws(broken((d) => { d.jobs.verify.if = d.jobs.verify.if.replace(" }}", " || true }}"); }), /condition must be exactly/);
  assert.doesNotThrow(broken((d) => { d.jobs.verify.if = `  ${d.jobs.verify.if}  `; }));
  for (const form of ["${{ secrets.X }}", "${{ secrets['X'] }}", '${{ secrets["X"] }}', "${{ toJSON(secrets) }}", "${{ secrets }}", "${{ SECRETS.X }}", "${{ format('{0}', secrets.X) }}"]) {
    for (const job of ["guard", "scans", "plan", "build-swap", "sign-swap", "build-credentials-helper", "sign-credentials-helper", "pack", "verify", "qualified"]) {
      assert.throws(broken((d) => { const step = d.jobs[job].steps.at(-1); step.env = { ...(step.env ?? {}), T: form }; }), /secrets context|secret/, `${job}: ${form}`);
    }
  }
  assert.throws(broken((d) => { d.jobs.verify.secrets = "inherit"; }), /secrets/);
  assert.match(realDoc().jobs.guard.steps[1].run, /exit 1\nfi\n$/);
});

test("reviewed promotion needs one dispatch, secretless admission & original-byte upload with exact approvals", () => {
  const doc = parseYaml(fs.readFileSync(new URL("../../.github/workflows/publish-reviewed.yml", import.meta.url), "utf8"));
  assert.equal(assertReviewedDoc(doc), true);
  assert.deepEqual(Object.keys(doc.on.workflow_dispatch.inputs), ["candidate_id", "dry_run"]);
  assert.equal(doc.on.workflow_dispatch.inputs.dry_run.default, "true");
  assert.deepEqual(doc.concurrency, { group: "publish", queue: "max", "cancel-in-progress": "false" });
  assert.equal(doc.jobs["seal-check"].steps[6].run, "node verifier/scripts/ci/reviewed-candidate.mjs run --dry-run");
  assert.equal(doc.jobs.upload.steps[2].run, "node scripts/ci/reviewed-candidate.mjs run --verify-admission");
  assert.deepEqual(doc.jobs.upload.needs, ["seal-check"]);
  assert.equal(doc.jobs.upload.if.includes("inputs.dry_run"), false);
  for (const [name, mutate] of REVIEWED_MUTATIONS) {
    const copy = structuredClone(doc); mutate(copy);
    assert.throws(() => assertReviewedDoc(copy), undefined, name);
  }
});

// ---- shipped-byte consumer verification ------------------------------------------------------

test("package.json entry points are enumerated from exports, main, types and bin", () => {
  const d = declaredEntries({ name: "@rightkit/x", main: "dist/index.js", types: "dist/index.d.ts", bin: { x: "./cli/x.mjs" }, exports: { ".": { types: "./dist/index.d.ts", import: "./dist/index.js" }, "./styles.css": "./styles.css", "./only-types": { types: "./t.d.ts" }, "./feature/*": "./dist/feature/*.js", "./package.json": "./package.json" } });
  assert.deepEqual(d.subpaths.map((s) => [s.spec, s.runtime, s.types]), [
    ["@rightkit/x", ["dist/index.js"], ["dist/index.d.ts"]], ["@rightkit/x/styles.css", ["styles.css"], []], ["@rightkit/x/only-types", [], ["t.d.ts"]], ["@rightkit/x/package.json", ["package.json"], []],
  ]);
  assert.deepEqual(d.patterns, ["./feature/*"]);
  assert.deepEqual(d.bins, ["cli/x.mjs"]);
  for (const f of ["dist/index.js", "dist/index.d.ts", "cli/x.mjs", "styles.css", "t.d.ts"]) assert.ok(d.files.includes(f), f);
  assert.deepEqual(declaredEntries({ name: "p", main: "lib/a.mjs" }).subpaths.map((s) => [s.spec, s.runtime]), [["p", ["lib/a.mjs"]]]);
  assert.equal(declaredEntries({ name: "p", bin: "cli.mjs" }).subpaths.length, 0);
  assert.equal(declaredEntries({ name: "p", exports: { import: "./i.js", require: "./r.cjs" } }).subpaths[0].runtime.length, 2, "a conditions-only exports object is the root entry");
  const declared = declaredEntries({ name: "p", exports: { ".": "./a.js" } });
  assert.deepEqual(skipImportList({ name: "p", rightkit: { publishCheck: { skipImport: ["."] } } }, declared), { list: ["."], problems: [] });
  assert.match(skipImportList({ name: "p", rightkit: { publishCheck: { skipImport: ["./nope"] } } }, declared).problems.join(), /does not export/);
  assert.match(skipImportList({ name: "p", rightkit: { publishCheck: { skipImport: "." } } }, declared).problems.join(), /array/);
  assert.deepEqual(skipImportList({ name: "p" }, declared), { list: [], problems: [] });
});

test("consumeNpm runs each installed bin's declared safe command and requires its exit code", () => {
  const withBin = (smoke, extra = {}) => sealNpm({ extraFiles: { "bin/tool.mjs": "console.log(1);\n" }, packageJson: (name, dir) => npmPackageJson(name, dir, { bin: { tool: "./bin/tool.mjs" }, ...(smoke ? { rightkit: { publishCheck: { binSmoke: smoke } } } : {}), ...extra }) });
  const run = (sealed, spawnFake) => consumeNpm({ units: sealed.manifest.units, packDir: sealed.dir, scratchRoot: tmp("scratch"), spawn: spawnFake.spawn, env: {}, npm: { command: "fake-npm", prefix: [], shell: false }, node: "fake-node" }).get("@rightkit/a");
  // declared and passing: the bin runs from the installed copy, with node, no shell, the declared arguments
  const good = consumerSpawn();
  const ok = run(withBin({ tool: ["--version"] }), good);
  assert.deepEqual(ok.problems, []);
  const call = good.calls.find((c) => c.bin);
  assert.deepEqual(call.args.slice(1), ["--version"]);
  assert.ok(call.args[0].includes(`node_modules${path.sep}@rightkit${path.sep}a${path.sep}bin${path.sep}tool.mjs`), "the INSTALLED copy runs");
  assert.notEqual(call.options.shell, true);
  assert.deepEqual(ok.report.binSmoke, [{ bin: "tool", args: ["--version"], status: 0 }]);
  // a bin that exits non-zero fails the unit, with its output
  const broken = run(withBin({ tool: ["--version"] }), consumerSpawn({ bin: () => ({ status: 1, stdout: "", stderr: "Error: Cannot find module 'x'" }) }));
  assert.match(broken.problems.join(), /installed bin tool --version exited 1 \(expected 0\): Error: Cannot find module 'x'/);
  // a bin with no declaration, a declaration for a bin that does not exist, and a malformed one all fail
  assert.match(run(withBin(null), consumerSpawn()).problems.join(), /declares bin tool but no rightkit\.publishCheck\.binSmoke/);
  assert.match(run(withBin({ tool: ["--version"], ghost: ["--help"] }), consumerSpawn()).problems.join(), /names ghost, which package\.json does not declare as a bin/);
  assert.match(run(withBin({ tool: "--version" }), consumerSpawn()).problems.join(), /no argument array for bin tool/);
  // a CLI whose only safe call is its usage message declares the exit code and the text it must print
  const usage = { tool: { args: [], exitCode: 64, output: "usage: tool" } };
  assert.deepEqual(run(withBin(usage), consumerSpawn({ bin: () => ({ status: 64, stdout: "", stderr: "usage: tool <x>\n" }) })).problems, []);
  assert.match(run(withBin(usage), consumerSpawn({ bin: () => ({ status: 0, stdout: "", stderr: "" }) })).problems.join(), /exited 0 \(expected 64\)/);
  assert.match(run(withBin(usage), consumerSpawn({ bin: () => ({ status: 64, stdout: "", stderr: "something else" }) })).problems.join(), /did not print "usage: tool"/);
  // a string-form bin is named after the package
  assert.deepEqual(declaredEntries({ name: "@rightkit/a", bin: "./bin/tool.mjs" }).binMap, { a: "bin/tool.mjs" });
});

test("npm CLI is run through node when it ships beside it, else through the shim", () => {
  assert.deepEqual(npmCliCommand({ execPath: "/n/bin/node", platform: "linux", exists: (p) => p.replaceAll("\\", "/") === "/n/lib/node_modules/npm/bin/npm-cli.js" }).prefix.length, 1);
  assert.deepEqual(npmCliCommand({ execPath: "C:\\n\\node.exe", platform: "win32", exists: () => false }), { command: "npm.cmd", prefix: [], shell: true });
  assert.deepEqual(npmCliCommand({ execPath: "/n/bin/node", platform: "darwin", exists: () => false }), { command: "npm", prefix: [], shell: false });
});

/** A fake npm + node: `install` unpacks each tarball into node_modules/<name>; the check script answers from `answer`. */
function consumerSpawn({ installStatus = 0, answer = (plan) => plan.items.map((i) => ({ spec: i.spec, ok: true, imported: !i.expectBlocked && i.import, ...(i.expectBlocked ? { rejected: true } : {}) })), realChecks = false, bin = () => ({ status: 0, stdout: "1.0.0\n", stderr: "" }) } = {}) {
  const calls = [];
  const spawn = (command, args, options = {}) => {
    calls.push({ command, args: [...args], options });
    if (command === "fake-npm") {
      if (installStatus) return { status: installStatus, stdout: "", stderr: "E404 not found" };
      for (const tgz of args.filter((a) => a.endsWith(".tgz"))) {
        const manifest = JSON.parse(spawnSync("tar", ["-xOzf", path.basename(tgz), "package/package.json"], { cwd: path.dirname(tgz), encoding: "utf8" }).stdout);
        const dest = path.join(options.cwd, "node_modules", ...manifest.name.split("/"));
        fs.mkdirSync(dest, { recursive: true });
        assert.equal(spawnSync("tar", ["-xzf", path.basename(tgz), "--strip-components=1", "-C", tarDir(dest)], { cwd: path.dirname(tgz) }).status, 0);
      }
      return { status: 0, stdout: "", stderr: "" };
    }
    const scriptIndex = args.findIndex((a) => !a.startsWith("--conditions="));
    if (command === "fake-node" && !path.basename(args[scriptIndex]).startsWith("check-")) {
      // an installed bin run for its smoke command
      calls.at(-1).bin = true;
      return bin(path.basename(args[0]), args.slice(1));
    }
    if (command === "fake-node") {
      const plan = JSON.parse(fs.readFileSync(args[scriptIndex + 1], "utf8"));
      calls.at(-1).plan = plan;
      if (realChecks) return spawnSync(process.execPath, args, options);
      return { status: 0, stdout: `noise\nRK_CONSUME ${JSON.stringify(answer(plan))}\n`, stderr: "" };
    }
    throw new Error(`unexpected child process: ${command}`);
  };
  return { spawn, calls };
}

test("consumeNpm installs every sealed tarball together with --ignore-scripts and imports each entry from the installed copy", () => {
  const sealed = sealNpm({ units: [["@rightkit/a", "packages/a"], ["@rightkit/b", "packages/b"]] });
  const { spawn, calls } = consumerSpawn();
  const result = consumeNpm({ units: sealed.manifest.units, packDir: sealed.dir, scratchRoot: tmp("scratch"), spawn, env: { PATH: "x" }, npm: { command: "fake-npm", prefix: [], shell: false }, node: "fake-node" });
  const install = calls.find((c) => c.command === "fake-npm");
  assert.deepEqual(install.args.slice(0, 5), ["install", "--ignore-scripts", "--no-audit", "--no-fund", "--no-package-lock"]);
  assert.deepEqual(install.args.slice(5).map((a) => path.basename(a)), sealed.manifest.units.map((u) => u.file), "one install of all tarballs, so they resolve each other");
  assert.equal(install.options.env.npm_config_userconfig.endsWith("npmrc"), true);
  for (const unit of sealed.manifest.units) {
    const r = result.get(unit.name);
    assert.deepEqual(r.problems, []);
    assert.deepEqual(r.report.imported, [unit.name]);
  }
  // an entry that throws, one that does not resolve, a missing file and a failed install are all problems
  const failing = consumerSpawn({ answer: (plan) => plan.items.map((i) => ({ spec: i.spec, ok: false, imported: false, error: "ReferenceError: window is not defined" })) });
  const bad = consumeNpm({ units: sealed.manifest.units, packDir: sealed.dir, scratchRoot: tmp("scratch"), spawn: failing.spawn, env: {}, npm: { command: "fake-npm", prefix: [], shell: false }, node: "fake-node" });
  assert.match(bad.get("@rightkit/a").problems.join(), /@rightkit\/a did not resolve or import: ReferenceError: window is not defined/);
  const dead = consumerSpawn({ installStatus: 1 });
  const none = consumeNpm({ units: sealed.manifest.units, packDir: sealed.dir, scratchRoot: tmp("scratch"), spawn: dead.spawn, env: {}, npm: { command: "fake-npm", prefix: [], shell: false }, node: "fake-node" });
  assert.match(none.get("@rightkit/b").problems.join(), /npm install --ignore-scripts of the sealed tarballs failed: E404/);
  const noFile = sealNpm({ packageJson: (name, dir) => npmPackageJson(name, dir, { main: "dist/gone.js", exports: { ".": "./dist/index.js" } }) });
  const m = consumeNpm({ units: noFile.manifest.units, packDir: noFile.dir, scratchRoot: tmp("scratch"), spawn: consumerSpawn().spawn, env: {}, npm: { command: "fake-npm", prefix: [], shell: false }, node: "fake-node" });
  assert.match(m.get("@rightkit/a").problems.join(), /dist\/gone\.js, which the installed package does not contain/);
});

test("a package declares skipImport for subpaths that need a DOM host; the entry is still resolved", () => {
  const sealed = sealNpm({ packageJson: (name, dir) => npmPackageJson(name, dir, { rightkit: { publishCheck: { skipImport: ["."] } } }) });
  const { spawn, calls } = consumerSpawn();
  const result = consumeNpm({ units: sealed.manifest.units, packDir: sealed.dir, scratchRoot: tmp("scratch"), spawn, env: {}, npm: { command: "fake-npm", prefix: [], shell: false }, node: "fake-node" });
  assert.deepEqual(result.get("@rightkit/a").problems, []);
  assert.deepEqual(result.get("@rightkit/a").report.resolvedOnly, ["@rightkit/a"]);
  assert.equal(calls.find((c) => c.command === "fake-node").plan.items[0].import, false);
});

test("sealed test-only exports reject production imports & execute only with declared test conditions", () => {
  const fixture = (testing = { development: { types: "./dist/testing.d.ts", import: "./dist/testing.js" }, default: null }, extraFiles = {}, extra = {}) => sealNpm({
    packageJson: (name, dir) => npmPackageJson(name, dir, { type: "module", exports: { ".": { import: "./dist/index.js" }, "./testing": testing }, ...extra }),
    extraFiles: { "dist/testing.js": "export const mock = true;\n", "dist/testing.d.ts": "export declare const mock: true;\n", ...extraFiles },
  });
  const run = (sealed) => {
    const fake = consumerSpawn({ realChecks: true });
    const result = consumeNpm({ units: sealed.manifest.units, packDir: sealed.dir, scratchRoot: tmp("scratch"), spawn: fake.spawn, env: { ...process.env, NODE_OPTIONS: "--conditions=development", NODE_ENV: "development" }, npm: { command: "fake-npm", prefix: [], shell: false }, node: "fake-node" }).get("@rightkit/a");
    return { ...result, calls: fake.calls };
  };
  const good = run(fixture());
  assert.deepEqual(good.problems, []);
  assert.deepEqual(good.report.imported, ["@rightkit/a"]);
  assert.deepEqual(good.report.productionRejected, ["@rightkit/a/testing"]);
  assert.deepEqual(good.report.conditionalImports, [{ spec: "@rightkit/a/testing", conditions: ["development"] }]);
  assert.equal(good.report.filesChecked, 3, "runtime & declaration files remain in closure");
  const checks = good.calls.filter((c) => c.command === "fake-node");
  assert.equal(checks.length, 2);
  assert.equal(checks[0].options.env.NODE_OPTIONS, "");
  assert.equal(checks[0].options.env.NODE_ENV, "production");
  assert.equal(checks[1].args[0], "--conditions=development");
  assert.equal(checks[1].plan.items[0].import, true);
  // A public default is a release leak, never a reason to treat testing as an ordinary entry.
  assert.match(run(fixture({ development: { import: "./dist/testing.js" }, default: "./dist/testing.js" })).problems.join(), /test-only export imported under production conditions/);
  assert.match(run(fixture("./dist/testing.js")).problems.join(), /test-only export has no development or test runtime condition/);
  // Only export-map rejection counts: missing files, throws, & missing transitive dependencies fail.
  assert.match(run(fixture({ development: { import: "./dist/missing.js" }, default: null })).problems.join(), /installed package.json points at dist\/missing.js/);
  assert.match(run(fixture({ development: { types: "./dist/missing.d.ts", import: "./dist/testing.js" }, default: null })).problems.join(), /installed package.json points at dist\/missing.d.ts/);
  assert.match(run(fixture(undefined, { "dist/testing.js": "throw new Error('broken test helper');\n" })).problems.join(), /broken test helper/);
  assert.match(run(fixture(undefined, { "dist/testing.js": "import 'rightkit-missing-fixture-dependency';\n" })).problems.join(), /rightkit-missing-fixture-dependency/);
  assert.match(run(fixture(undefined, { "dist/index.js": "throw new Error('ordinary entry failed');\n" })).problems.join(), /ordinary entry failed/);
  assert.match(run(fixture(undefined, { "dist/testing.js": "throw new Error('skipImport cannot hide this');\n" }, { rightkit: { publishCheck: { skipImport: ["./testing"] } } })).problems.join(), /skipImport cannot hide this/);
  assert.match(run(fixture({ development: { import: "./dist/testing.js" }, default: "./dist/testing.js" }, { "dist/testing.js": "throw Object.assign(new Error('dependency not exported'), { code: 'ERR_PACKAGE_PATH_NOT_EXPORTED' });\n" })).problems.join(), /test-only export resolves under production conditions/);
  assert.deepEqual(run(fixture({ test: { import: "./dist/testing.js" }, default: null })).problems, []);
});

test("consumeCrates qualifies each selected exact archive through its committed registry PK example", () => {
  const sealed = sealCrates(["rightkit-a", "rightkit-b"]);
  const root = tmp("crates-consume");
  const checks = [];
  const registryConsumer = options => {
    checks.push(options);
    const unit = sealed.manifest.units.find(u => u.name === options.name);
    return { qualification: "R2a", verified: true, name: unit.name, version: unit.version, archiveSha256: unit.sha256, recipe: { id: `ci/consumers/${unit.name}/src/main.rs` } };
  };
  const spawn = () => assert.fail("generic Cargo source checks cannot qualify");
  const result = consumeCrates({ units: sealed.manifest.units, packDir: sealed.dir, scratchRoot: root, spawn, registryConsumer, uploadMetadata: buildCrateUploadMetadata });
  assert.deepEqual([...result.values()].flatMap((r) => r.problems), []);
  assert.equal(checks.length, 2);
  assert.deepEqual(checks.map(c => c.name), ["rightkit-a", "rightkit-b"]);
  assert.ok(checks.every(c => c.packDir === sealed.dir && c.uploadMetadata === buildCrateUploadMetadata && c.spawn === spawn));
  checks.length = 0;
  const excluded = consumeCrates({ units: sealed.manifest.units, packDir: sealed.dir, scratchRoot: root, spawn, registryConsumer, excluded: ["rightkit-a", "rightkit-b"] });
  assert.equal(checks.length, 0);
  assert.equal(excluded.get("rightkit-a").report.checked, false);
});

test("missing recipes, wrong archive proof & PK runtime failure fail selected units without fallback", () => {
  const units = [{ name: "rightkit-a", version: "0.1.0", sha256: "a".repeat(64) }, { name: "rightkit-b", version: "0.1.0", sha256: "b".repeat(64) }];
  for (const registryConsumer of [() => { throw new Error("missing committed PK example ci/consumers/rightkit-a/src/main.rs"); }, () => ({ qualification: "R2a", verified: true, name: "rightkit-a", version: "0.1.0", archiveSha256: "f".repeat(64) }), () => { const error = new Error("PK runtime failed"); error.report = { qualification: "R2a", verified: false, commands: [{ command: "cargo", args: ["run", "--locked", "--offline"], status: 101 }] }; throw error; }]) {
    const result = consumeCrates({ units, packDir: "unused", registryConsumer, spawn: () => assert.fail("must never fallback to generic check") });
    assert.ok(result.get("rightkit-a").problems.length);
    assert.ok(result.get("rightkit-b").problems.length);
  }
});

// ---- assets, allow inputs and Chrome for Testing -------------------------------------------------

test("a unit that needs unprovisioned assets must be named in allow_missing_assets, and only such units may be", async () => {
  const root = fixture();
  fs.writeFileSync(path.join(root, "ci", "asset-gated.json"), JSON.stringify({ crates: { "rightkit-asr": "models" }, npm: { "@rightkit/qa": "checkouts" } }));
  const workspace = new Map([crate("rightkit-asr", "0.1.0"), crate("rightkit-plain", "0.1.0")]);
  await assert.rejects(planPublication({ registry: "crates", unitsText: "rightkit-asr", dryRun: true, root, workspace, fetchImpl: registry({}) }), /rightkit-asr: its tests need assets this workflow does not provision \(models\); name it in allow_missing_assets/);
  const ok = await planPublication({ registry: "crates", unitsText: "rightkit-asr rightkit-plain", allowMissingAssetsText: "rightkit-asr", allowIgnoredText: "rightkit-plain", dryRun: true, root, workspace, fetchImpl: registry({}) });
  assert.deepEqual([ok.allowMissingAssets, ok.allowIgnored], [["rightkit-asr"], ["rightkit-plain"]]);
  await assert.rejects(planPublication({ registry: "crates", unitsText: "rightkit-plain", allowMissingAssetsText: "rightkit-plain", dryRun: true, root, workspace, fetchImpl: registry({}) }), /allow_missing_assets names rightkit-plain, which ci\/asset-gated\.json does not list/);
  await assert.rejects(planPublication({ registry: "crates", unitsText: "rightkit-plain", allowIgnoredText: "rightkit-other", dryRun: true, root, workspace, fetchImpl: registry({}) }), /allow_ignored names .*rightkit-other.*not units of this request/);
});

test("ci/asset-gated.json lists every crate and package whose tests read RIGHTKIT_REQUIRE_ASSETS", () => {
  const repo = fileURLToPath(new URL("../..", import.meta.url));
  const gated = JSON.parse(fs.readFileSync(path.join(repo, "ci", "asset-gated.json"), "utf8"));
  const provisionedNpm = new Set(["@rightkit/qa", "@rightkit/qa-chrome"]);
  const publishWorkflow = fs.readFileSync(path.join(repo, ".github", "workflows", "publish.yml"), "utf8");
  const gateSource = fs.readFileSync(path.join(repo, "scripts", "ci", "gate.mjs"), "utf8");
  assert.equal(parseYaml(publishWorkflow).jobs.verify.steps.find((step) => step.name === "Provision the pinned Chrome for Testing").if, CHROME_IF);
  for (const name of provisionedNpm) assert.ok(CHROME_IF.includes(`' ${name} '`), `${name} requires explicit Chrome provisioning`);
  assert.match(gateSource, /RIGHTKIT_REQUIRE_ASSETS:\s*"1"/, "unallowed package test runs keep strict required-assets mode");
  const reads = (dir) => {
    const hits = [];
    const walk = (d) => { for (const e of fs.readdirSync(d, { withFileTypes: true })) { if (["node_modules", "target", "dist", ".git"].includes(e.name)) continue; const full = path.join(d, e.name); if (e.isDirectory()) walk(full); else if (/\.(rs|mjs|ts|tsx|js)$/.test(e.name) && fs.readFileSync(full, "utf8").includes("RIGHTKIT_REQUIRE_ASSETS")) hits.push(full); } };
    walk(dir);
    return hits.length > 0;
  };
  for (const entry of fs.readdirSync(path.join(repo, "crates"), { withFileTypes: true })) {
    if (!entry.isDirectory()) continue;
    const toml = path.join(repo, "crates", entry.name, "Cargo.toml");
    if (!fs.existsSync(toml)) continue;
    const name = /^\[package\][^[]*?^name\s*=\s*"([^"]+)"/ms.exec(fs.readFileSync(toml, "utf8"))?.[1];
    if (reads(path.join(repo, "crates", entry.name))) assert.ok(Object.hasOwn(gated.crates, name), `${name} reads RIGHTKIT_REQUIRE_ASSETS but is not in ci/asset-gated.json`);
  }
  for (const entry of fs.readdirSync(path.join(repo, "packages"), { withFileTypes: true })) {
    if (!entry.isDirectory()) continue;
    const manifest = path.join(repo, "packages", entry.name, "package.json");
    if (!fs.existsSync(manifest)) continue;
    const name = JSON.parse(fs.readFileSync(manifest, "utf8")).name;
    if (reads(path.join(repo, "packages", entry.name)) && !Object.hasOwn(gated.npm, name)) {
      assert.ok(provisionedNpm.has(name), `${name} reads RIGHTKIT_REQUIRE_ASSETS but is neither asset-gated nor provisioned by publish.yml`);
    }
  }
});

test("the Chrome for Testing pin: URL derives from the version; an empty sha256 can only be recorded in a dry run", async () => {
  const pins = { version: "130.0.6723.58", platforms: { win64: { sha256: "a".repeat(64) }, "mac-arm64": { sha256: "" } } };
  assert.deepEqual(pinFor(pins, "win64"), { url: "https://storage.googleapis.com/chrome-for-testing-public/130.0.6723.58/win64/chrome-win64.zip", sha256: "a".repeat(64) });
  assert.equal(pinFor(pins, "mac-arm64").sha256, "");
  assert.throws(() => pinFor({ version: "", platforms: {} }, "win64"), /no pinned version/);
  assert.throws(() => pinFor(pins, "linux64"), /no entry for linux64/);
  assert.throws(() => pinFor({ ...pins, platforms: { win64: { sha256: "XYZ" } } }, "win64"), /not 64 lowercase hex/);
  const body = Buffer.from("chrome bytes");
  const fetchImpl = async (url) => ({ ok: true, status: 200, body: (async function* () { yield body.subarray(0, 5); yield body.subarray(5); })(), url });
  const tmpDir = tmp("pin");
  const line = await recordPin({ version: pins.version, key: "mac-arm64", url: pinFor(pins, "mac-arm64").url, dryRun: true, fetchImpl, tmpDir });
  assert.equal(line, `CHROME_PIN mac-arm64 130.0.6723.58 ${sha256(body)} ${body.length}`);
  assert.equal(fs.existsSync(tmpDir), false, "the downloaded file is deleted");
  await assert.rejects(recordPin({ version: pins.version, key: "mac-arm64", url: "u", dryRun: false, fetchImpl: async () => { throw new Error("must not download"); }, tmpDir: tmp("pin") }), /not a dry run/);
  const real = JSON.parse(fs.readFileSync(new URL("../../ci/chrome-for-testing.json", import.meta.url), "utf8"));
  assert.deepEqual(Object.keys(real.platforms).sort(), ["linux64", "mac-arm64", "win64"]);
});

// ---- Linux verification (ci/linux-required.json): a required Linux row is never optional -------------------------------

const LINUX_UNIT = "@rightkit/qa-chrome"; // listed in the real ci/linux-required.json
const sealLinux = () => sealNpm({ units: [["@rightkit/a", "packages/a"], [LINUX_UNIT, "packages/qa-chrome"]] });

/** Receipts for chosen hosts; the linux receipt covers only `linuxUnits`, the native ones every sealed unit. */
function hostReceipts(packDir, manifest, { hosts = ["macos", "windows", "linux"], linuxUnits = [LINUX_UNIT], rowOver = {}, docOver = {} } = {}) {
  const dir = tmp("linux-receipts");
  const manifestSha256 = sha256(fs.readFileSync(path.join(packDir, "manifest.json")));
  for (const host of hosts) {
    const scope = host === "linux" ? manifest.units.filter((u) => linuxUnits.includes(u.name)) : manifest.units;
    fs.mkdirSync(path.join(dir, `publish-receipt-verify-${host}`), { recursive: true });
    const doc = {
      schema: 1, kind: "verify", host, registry: manifest.registry, commit: manifest.commit, manifestSha256, runId: "77", runAttempt: "1", manifestRunId: manifest.runId, manifestRunAttempt: manifest.runAttempt,
      allowHostExclusion: [], allowNoTest: [], allowIgnored: [], allowMissingAssets: [],
      units: scope.map((u) => ({ name: u.name, version: u.version, file: u.file, sha256: u.sha256, archiveScan: archiveScanFixture(u), bytesVerified: true, status: "pass", executed: 3, ignored: 0, skipped: 0, consumer: { verified: true }, ...(host === "linux" ? rowOver : {}) })),
      ...(host === "linux" ? docOver : {}),
    };
    fs.writeFileSync(path.join(dir, `publish-receipt-verify-${host}`, `verify-${host}.json`), JSON.stringify(doc));
  }
  return dir;
}
const receiptProblems = (sealed, receiptsDir, extra = {}) => checkReceipts({ manifest: sealed.manifest, manifestSha256: sha256(fs.readFileSync(path.join(sealed.dir, "manifest.json"))), receiptsDir, run: { runId: "77", runAttempt: "1" }, ...extra });

test("Linux required: a missing, partial, wrong-host or non-pass Linux receipt is refused; the exact set passes", () => {
  const sealed = sealLinux();
  assert.deepEqual(receiptProblems(sealed, hostReceipts(sealed.dir, sealed.manifest)), [], "native + Linux receipts for exactly the required set pass");
  // missing: both native hosts passing never stands in for the Linux row
  const missing = receiptProblems(sealed, hostReceipts(sealed.dir, sealed.manifest, { hosts: ["macos", "windows"] }));
  assert.ok(missing.some((p) => /expected exactly one verify receipt from linux, found 0/.test(p)), JSON.stringify(missing));
  // duplicate
  const dup = hostReceipts(sealed.dir, sealed.manifest);
  fs.mkdirSync(path.join(dup, "again"), { recursive: true });
  fs.copyFileSync(path.join(dup, "publish-receipt-verify-linux", "verify-linux.json"), path.join(dup, "again", "verify-linux.json"));
  assert.ok(receiptProblems(sealed, dup).some((p) => /from linux, found 2/.test(p)));
  // a Linux receipt that covers no required unit, or an extra one
  assert.ok(receiptProblems(sealed, hostReceipts(sealed.dir, sealed.manifest, { linuxUnits: [] })).some((p) => /linux receipt: covers 0 units/.test(p)));
  assert.ok(receiptProblems(sealed, hostReceipts(sealed.dir, sealed.manifest, { linuxUnits: [LINUX_UNIT, "@rightkit/a"] })).some((p) => /linux receipt: covers 2 units/.test(p)));
  assert.ok(receiptProblems(sealed, hostReceipts(sealed.dir, sealed.manifest, { linuxUnits: ["@rightkit/a"] })).some((p) => /linux receipt: covers 1 units/.test(p) || /linux receipt: no evidence for @rightkit\/qa-chrome/.test(p)));
  // non-pass rows: Linux allows none of the native allowances
  for (const status of ["allowed-exclusion", "allowed-no-test", "allowed-skips", "fail", "missing"]) {
    const problems = receiptProblems(sealed, hostReceipts(sealed.dir, sealed.manifest, { rowOver: { status } }));
    assert.ok(problems.some((p) => p.startsWith("linux receipt:")), `${status}: ${JSON.stringify(problems)}`);
  }
  assert.ok(receiptProblems(sealed, hostReceipts(sealed.dir, sealed.manifest, { rowOver: { executed: 0 } })).some((p) => /linux receipt: .*status pass without a positive executed count/.test(p)));
  assert.ok(receiptProblems(sealed, hostReceipts(sealed.dir, sealed.manifest, { rowOver: { consumer: { verified: false } } })).some((p) => /linux receipt: .*not consumer-verified/.test(p)));
  assert.ok(receiptProblems(sealed, hostReceipts(sealed.dir, sealed.manifest, { rowOver: { sha256: "b".repeat(64) } })).some((p) => /linux receipt: .*different bytes/.test(p)));
  assert.ok(receiptProblems(sealed, hostReceipts(sealed.dir, sealed.manifest, { docOver: { commit: "c".repeat(40) } })).some((p) => /linux receipt: commit/.test(p)));
  assert.ok(receiptProblems(sealed, hostReceipts(sealed.dir, sealed.manifest, { docOver: { manifestSha256: "d".repeat(64) } })).some((p) => /linux receipt: it verified a different manifest/.test(p)));
  assert.ok(receiptProblems(sealed, hostReceipts(sealed.dir, sealed.manifest, { docOver: { allowNoTest: [LINUX_UNIT] } })).some((p) => /linux receipt: it ran with different allow_no_test/.test(p)));
});

test("Linux not required: verification is unchanged and a stray Linux receipt is refused", () => {
  const sealed = sealNpm();
  assert.deepEqual(receiptProblems(sealed, receipts(sealed.dir, sealed.manifest)), [], "macos + windows only, as before");
  assert.ok(receiptProblems(sealed, receipts(sealed.dir, sealed.manifest, { skip: ["windows"] })).some((p) => /from windows, found 0/.test(p)), "native hosts still required");
  const stray = hostReceipts(sealed.dir, sealed.manifest, { linuxUnits: ["@rightkit/a"] });
  const problems = receiptProblems(sealed, stray);
  assert.ok(problems.some((p) => /unexpected verify receipt from "linux"; the required hosts are macos, windows/.test(p)), JSON.stringify(problems));
  // a crates run lists no Linux unit
  const crates = sealCrates(["rightkit-a"]);
  assert.deepEqual(receiptProblems(crates, receipts(crates.dir, crates.manifest)), []);
});

test("the receipt host set equals the required host set", () => {
  const sealed = sealLinux();
  const hostsOf = (dir) => fs.readdirSync(dir).map((d) => d.replace("publish-receipt-verify-", "")).sort();
  const required = (linuxRequired) => requiredHostsFor(linuxRequired ?? linuxRequiredFor({ registry: "npm", units: sealed.manifest.units.map((u) => u.name) }).units).sort();
  assert.deepEqual(required(), ["linux", "macos", "windows"]);
  const exact = hostReceipts(sealed.dir, sealed.manifest);
  assert.deepEqual(hostsOf(exact), required());
  assert.deepEqual(receiptProblems(sealed, exact), []);
  // caller-pinned requirement: none -> exactly two hosts; the Linux receipt is then surplus and refused
  assert.deepEqual(requiredHostsFor([]).sort(), ["macos", "windows"]);
  assert.ok(receiptProblems(sealed, exact, { linuxRequired: [] }).some((p) => /unexpected verify receipt from "linux"/.test(p)));
  assert.ok(receiptProblems(sealed, hostReceipts(sealed.dir, sealed.manifest, { hosts: ["macos", "windows"] }), { linuxRequired: [LINUX_UNIT] }).some((p) => /from linux, found 0/.test(p)));
  // any other host is refused outright
  const odd = hostReceipts(sealed.dir, sealed.manifest, { hosts: ["macos", "windows", "linux", "freebsd"] });
  assert.ok(receiptProblems(sealed, odd).some((p) => /unexpected verify receipt from "freebsd"/.test(p)));
});

test("the Linux requirement list fails closed when unreadable and derives from the reviewed file", () => {
  assert.deepEqual(linuxRequiredFor({ registry: "npm", units: ["@rightkit/a", LINUX_UNIT, "@rightkit/qa"] }), { units: [LINUX_UNIT], problems: [] });
  assert.deepEqual(linuxRequiredFor({ registry: "crates", units: ["rightkit-qa", "rightkit-fs"] }), { units: [], problems: [] });
  const write = (text) => { const file = path.join(tmp("linux-list"), "linux-required.json"); fs.writeFileSync(file, text); return file; };
  assert.equal(linuxRequiredFor({ registry: "npm", units: [LINUX_UNIT], file: path.join(tmp("none"), "absent.json") }).problems.length, 1);
  for (const bad of ["not json", "[]", "{}", JSON.stringify({ schema: 2, npm: [], crates: [] }), JSON.stringify({ schema: 1, npm: [LINUX_UNIT, LINUX_UNIT], crates: [] }), JSON.stringify({ schema: 1, npm: "x", crates: [] }), JSON.stringify({ schema: 1, npm: [] })]) {
    const result = linuxRequiredFor({ registry: "npm", units: [LINUX_UNIT], file: write(bad) });
    assert.equal(result.problems.length, 1, bad);
    assert.deepEqual(result.units, []);
  }
  // an unreadable list makes checkReceipts refuse rather than treat Linux as not required
  const sealed = sealLinux();
  const problems = checkReceipts({ manifest: sealed.manifest, manifestSha256: sha256(fs.readFileSync(path.join(sealed.dir, "manifest.json"))), receiptsDir: hostReceipts(sealed.dir, sealed.manifest, { hosts: ["macos", "windows"] }), run: { runId: "77", runAttempt: "1" }, linuxRequired: undefined });
  assert.ok(problems.some((p) => /from linux, found 0/.test(p)));
  // the committed list names a real, publishable npm package
  const real = JSON.parse(fs.readFileSync(new URL("../../ci/linux-required.json", import.meta.url), "utf8"));
  assert.deepEqual(Object.keys(real).sort(), ["_comment", "crates", "npm", "schema"]);
  assert.deepEqual(real.npm, [LINUX_UNIT]);
  const manifest = JSON.parse(fs.readFileSync(new URL("../../packages/qa-chrome/package.json", import.meta.url), "utf8"));
  assert.equal(manifest.name, LINUX_UNIT);
});

test("verify on Linux covers exactly the Linux-required units and refuses every other shape", async () => {
  const sealed = sealLinux();
  const runTemp = tmp("verify-linux");
  const evidenceDir = path.join(runTemp, "rightkit-ci");
  fs.mkdirSync(evidenceDir, { recursive: true });
  const evidence = (names, patch = {}) => {
    const rows = names.map((name) => { const unit = sealed.manifest.units.find((u) => u.name === name); const result = { unit: `npm:${unit.dir}`, command: `pnpm --filter ${name} run test`, isTest: true, code: 0, timedOut: false, seconds: 1, executed: 3, failed: 0, ignored: 0, skipped: 0, ok: true }; return { row: { name, status: "pass", executed: 3, steps: [result] }, result }; });
    fs.writeFileSync(path.join(evidenceDir, "unit-evidence.json"), JSON.stringify({ schema: 2, ok: true, host: "linux", registry: "npm", sha: SHA, dependentClosure: { selection: [], expected: [], results: [], ok: true, problems: [] }, units: rows.map((r) => r.row), ...patch }));
    fs.writeFileSync(path.join(evidenceDir, "summary.json"), JSON.stringify({ host: "linux", sha: SHA, results: rows.map((r) => r.result) }));
  };
  const env = actionsEnv({ RUNNER_TEMP: runTemp, PUBLISH_PACK_DIR: sealed.dir, GITHUB_ACTOR: "fixture-actor", GITHUB_JOB: "verify-linux", PUBLISH_UNITS: "@rightkit/a @rightkit/qa-chrome", PUBLISH_LINUX_UNITS: LINUX_UNIT });
  const consumed = [];
  const scanArchive = (options) => scanArchiveForSecrets({ ...options, scannerOwner: { packageFileViolations: () => [], scanCrateForSecrets: () => [] } });
  const consume = { npm: ({ units }) => { consumed.push(units.map((u) => u.name)); return new Map(units.map((u) => [u.name, { problems: [], report: { entries: 1 } }])); }, crates: () => { throw new Error("not used"); } };
  const run = (over = {}, host = "linux") => commandVerify({ env: { ...env, ...over }, spawn: spy({ git: () => ({ status: 0, stdout: `${SHA}\n` }) }).spawn, host, consume, scanArchive });
  evidence([LINUX_UNIT]);
  const receipt = await run();
  assert.equal(receipt.host, "linux");
  assert.deepEqual(receipt.units.map((u) => [u.name, u.status, u.bytesVerified, u.consumer.verified]), [[LINUX_UNIT, "pass", true, true]], "only the Linux-required unit has a row");
  assert.deepEqual(consumed, [[LINUX_UNIT]], "only the Linux-required unit is consumed");
  assert.ok(fs.existsSync(path.join(runTemp, "verify-receipt", "verify-linux.json")));
  // the receipt it writes is exactly what checkReceipts requires from Linux
  const receiptsDir = tmp("linux-roundtrip");
  fs.mkdirSync(path.join(receiptsDir, "publish-receipt-verify-linux"), { recursive: true });
  fs.copyFileSync(path.join(runTemp, "verify-receipt", "verify-linux.json"), path.join(receiptsDir, "publish-receipt-verify-linux", "verify-linux.json"));
  for (const host of ["macos", "windows"]) {
    fs.mkdirSync(path.join(receiptsDir, `publish-receipt-verify-${host}`), { recursive: true });
    const native = JSON.parse(fs.readFileSync(path.join(hostReceipts(sealed.dir, sealed.manifest, { hosts: [host] }), `publish-receipt-verify-${host}`, `verify-${host}.json`), "utf8"));
    fs.writeFileSync(path.join(receiptsDir, `publish-receipt-verify-${host}`, `verify-${host}.json`), JSON.stringify(native));
  }
  const roundtrip = receiptProblems(sealed, receiptsDir).filter((p) => !/^linux receipt: .*archiveScan/.test(p));
  assert.deepEqual(roundtrip.filter((p) => p.startsWith("linux receipt:")), [], JSON.stringify(roundtrip));
  // every wrong shape refuses
  evidence([LINUX_UNIT, "@rightkit/a"]);
  await assert.rejects(() => run(), /the gate ran \[@rightkit\/a @rightkit\/qa-chrome\] on Linux, not exactly the Linux-required units/);
  evidence([]);
  await assert.rejects(() => run(), /the gate ran \[\] on Linux/);
  evidence(["@rightkit/a"]);
  await assert.rejects(() => run(), /the gate ran \[@rightkit\/a\] on Linux/);
  evidence([LINUX_UNIT]);
  await assert.rejects(() => run({ PUBLISH_LINUX_UNITS: "" }), /the Linux units the plan named \[\] differ from the Linux-required sealed units \[@rightkit\/qa-chrome\]/);
  await assert.rejects(() => run({ PUBLISH_LINUX_UNITS: "@rightkit/a" }), /the Linux units the plan named \[@rightkit\/a\] differ/);
  evidence([LINUX_UNIT], { host: "windows" });
  await assert.rejects(() => run(), /unavailable or inconsistent/);
  evidence([LINUX_UNIT]);
  const evidenceFile = path.join(evidenceDir, "unit-evidence.json");
  const failing = JSON.parse(fs.readFileSync(evidenceFile, "utf8")); failing.units[0].status = "allowed-exclusion"; fs.writeFileSync(evidenceFile, JSON.stringify(failing));
  const summary = path.join(evidenceDir, "summary.json");
  await assert.doesNotReject(() => run(), "exclusion evidence is carried to checkReceipts, which refuses it on Linux");
  const excluded = JSON.parse(fs.readFileSync(path.join(runTemp, "verify-receipt", "verify-linux.json"), "utf8"));
  assert.equal(excluded.units[0].status, "allowed-exclusion");
  const exclusionDir = tmp("linux-exclusion");
  fs.mkdirSync(path.join(exclusionDir, "publish-receipt-verify-linux"), { recursive: true });
  fs.writeFileSync(path.join(exclusionDir, "publish-receipt-verify-linux", "verify-linux.json"), JSON.stringify(excluded));
  assert.ok(receiptProblems(sealed, exclusionDir).some((p) => /linux receipt: .*needs a real pass on Linux/.test(p)));
  // a Linux run with no Linux-required unit sealed refuses rather than writing an empty pass
  const plain = sealNpm();
  const plainEvidence = path.join(runTemp, "rightkit-ci");
  const plainRun = { ...env, PUBLISH_PACK_DIR: plain.dir, PUBLISH_UNITS: "@rightkit/a", PUBLISH_LINUX_UNITS: "" };
  fs.writeFileSync(path.join(plainEvidence, "unit-evidence.json"), JSON.stringify({ schema: 2, ok: true, host: "linux", registry: "npm", sha: SHA, units: [] }));
  fs.writeFileSync(path.join(plainEvidence, "summary.json"), JSON.stringify({ host: "linux", sha: SHA, results: [] }));
  await assert.rejects(() => commandVerify({ env: plainRun, spawn: spy({ git: () => ({ status: 0, stdout: `${SHA}\n` }) }).spawn, host: "linux", consume, scanArchive }), /no sealed unit requires Linux verification/);
  // macOS and Windows still verify every sealed unit (unchanged)
  evidence([LINUX_UNIT, "@rightkit/a"], { host: "windows" });
  fs.writeFileSync(path.join(evidenceDir, "summary.json"), JSON.stringify({ host: "windows", sha: SHA, results: JSON.parse(fs.readFileSync(summary, "utf8")).results }));
  const windows = await run({ PUBLISH_LINUX_UNITS: LINUX_UNIT }, "windows");
  assert.deepEqual(windows.units.map((u) => u.name), ["@rightkit/a", LINUX_UNIT]);
});

test("the plan exposes the Linux-required units to the workflow and refuses an unreadable list", { skip: realWorkflowSkip }, () => {
  const doc = realDoc();
  assert.equal(doc.jobs.plan.outputs.linux_count, "${{ steps.plan.outputs.linux_count }}");
  assert.equal(doc.jobs.plan.outputs.linux_units, "${{ steps.plan.outputs.linux_units }}");
  const linux = doc.jobs["verify-linux"];
  assert.equal(linux["runs-on"], "ubuntu-24.04");
  assert.equal(linux.strategy, undefined);
  assert.equal(linux.if.trim(), LINUX_IF);
  assert.deepEqual(linux.needs, ["guard", "scans", "plan", "pack"]);
  assert.equal(linux.steps.find((s) => /gate\.mjs/.test(s.run ?? "")).env.RK_SCANS_JOB_RESULT, "${{ needs.scans.result }}");
  assert.ok(doc.jobs.verify.needs.includes("scans"));
  assert.equal(doc.jobs.verify.steps.find((s) => /gate\.mjs/.test(s.run ?? "")).env.RK_SCANS_JOB_RESULT, "${{ needs.scans.result }}");
  assert.equal(linux.steps.find((s) => /gate\.mjs/.test(s.run ?? "")).run.trim(), 'node scripts/ci/gate.mjs --registry "$PUBLISH_REGISTRY" --units "$PUBLISH_LINUX_UNITS"');
  for (const name of ["Cache the pinned Chrome for Testing", "Provision the pinned Chrome for Testing"]) assert.equal(linux.steps.find((s) => s.name === name).if.trim(), LINUX_CHROME_IF);
  assert.match(LINUX_CHROME_IF, /needs\.plan\.outputs\.linux_units\), ' @rightkit\/qa-chrome '/);
  assert.doesNotMatch(LINUX_CHROME_IF, /needs\.plan\.outputs\.units/);
  // the aggregate reads the same receipt the job uploads, and the contract rejects every Linux mutation
  assert.equal(linux.steps.find((s) => s.with?.name === "publish-receipt-verify-ubuntu-24.04").with["if-no-files-found"], "error");
  const names = MUTATIONS.map(([name]) => name);
  for (const required of ["verify-linux is removed", "qualified stops needing verify-linux", "qualified always expects Linux skipped", "verify-linux gates every unit, not the Linux-required ones", "verify-linux skips the receipt", "verify-linux receipt artifact is optional", "verify-linux runs on Windows"]) assert.ok(names.includes(required), required);
  assert.deepEqual(proveMutations(doc), []);
  const pins = JSON.parse(fs.readFileSync(new URL("../../ci/chrome-for-testing.json", import.meta.url), "utf8"));
  assert.equal(pins.platforms.linux64.sha256, "e6c94490f0b6b337225aeb2d2a5034858516b7017356980ba94f5176d8774a50", "linux64 uses the exact reviewed qa-chrome installer pin");
  assert.equal(pinFor(pins, "linux64").sha256, pins.platforms.linux64.sha256);
});
