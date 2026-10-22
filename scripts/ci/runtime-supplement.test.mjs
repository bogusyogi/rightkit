import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { sha, REPOSITORY } from "./upload.mjs";
import { candidateHash } from "./supplemental-proof.mjs";
import { workloadsFor, supportedPrescription } from "./runtime-qualify.mjs";
import test from "node:test";
import { validateRuntimeWorkflow, authenticatedRuntimeProof, verifyRuntimeSupplement, validateFrozenRuntimeBudget, validateRuntimeRecords, trustedRuntimeHelperPaths } from "./runtime-supplement.mjs";
import { runtimeFeatureProofPolicy } from "./semantic-admission.mjs";
// The proof names its crate; this lane's fixture is rightkit-logs, passed explicitly to every helper.
const CRATE = "rightkit-logs";
test("runtime evidence rejects caller JSON before interpreting candidate", () => {
  assert.throws(() => authenticatedRuntimeProof({}, {}, {}), /unauthenticated/);
});
test("runtime measure workflow requires credential-free pinned producer & retained raw evidence", () => {
  const original = fs.readFileSync(new URL("../../.github/workflows/qualify-runtime.yml", import.meta.url), "utf8");
  validateRuntimeWorkflow(original);
  for (const mutate of [
    (s) => s.replace("contents: read", "contents: write"),
    (s) => s.replace("host: win32", "host: linux"),
    (s) => s.replace(/retention-days: 90/g, "retention-days: 14"),
    (s) => s.replace("node harness/scripts/ci/runtime-qualify.mjs measure-clean", "echo claimed-success"),
    (s) => s.replace("node harness/scripts/ci/runtime-qualify.mjs measure-clean", "node harness/scripts/ci/runtime-qualify.mjs measure-clean # secrets.TEST"),
  ].slice(0, 4)) assert.throws(() => validateRuntimeWorkflow(mutate(original)), /refused/);
});

const SOURCE = "a".repeat(40), PRODUCER = "b".repeat(40), VALIDATOR = "c".repeat(40);
const hash = (bytes) => sha("sha256", bytes);
function runtimeFixture(t, crate = CRATE) {
  // The fixture serves rightkit-logs (default) or rightkit-daemon; every crate-specific fact comes from this table.
  const daemon = crate === "rightkit-daemon", shell = crate === "rightkit-tauri-shell", version = shell ? "0.5.0" : daemon ? "0.4.0" : "0.3.4";
  const scaleOf = (w) => (shell ? { unit: "watchdogs", count: 1 } : daemon ? (w.id === "remote" ? { unit: "submissions", count: 10000 } : { unit: "connections", count: 64 }) : { unit: "lines", count: 200000 });
  const scratch = process.env.RIGHTKIT_TEST_SCRATCH ?? process.env.RUNNER_TEMP ?? os.tmpdir();
  const temp = fs.realpathSync.native(fs.mkdtempSync(path.join(scratch, "runtime-admission-fixture-")));
  t.after(() => fs.rmSync(temp, { recursive: true, force: true }));
  const archive = Buffer.from("original sealed crate fixture"), metadata = Buffer.from(`{"name":"${crate}","vers":"${version}","deps":[]}`);
  const unit = { name: crate, version, file: `${crate}-${version}.crate`, size: archive.length, sha256: hash(archive), metaFile: `${crate}-${version}.upload.json`, metaSha256: hash(metadata) };
  const manifest = { schema: 1, registry: "crates", commit: SOURCE, runId: "77", runAttempt: "1", units: [unit] }, rawManifest = Buffer.from(JSON.stringify(manifest));
  const zip = { packed: Buffer.from("packed ZIP"), supplement: Buffer.from("runtime ZIP") };
  const candidate = { id: "candidate-77", sourceSha: SOURCE, runId: "77", runAttempt: "1", workflowId: "5", manifestSha256: hash(rawManifest), registry: "crates", units: [unit.name], allow: { hostExclusion: [], noTest: [], ignored: [], missingAssets: [] }, artifacts: { packed: { id: "10", digest: `sha256:${hash(zip.packed)}` }, macos: { id: "11", digest: `sha256:${"d".repeat(64)}`, receiptSha256: "e".repeat(64) }, windows: { id: "12", digest: `sha256:${"f".repeat(64)}`, receiptSha256: "0".repeat(64) } } };
  const pin = { schema: 1, kind: "runtime-workload-pin", original: { candidateId: candidate.id, candidateSha256: candidateHash(candidate), sourceSha: SOURCE, manifestSha256: candidate.manifestSha256, packedArtifact: candidate.artifacts.packed }, producer: { workflowId: "6", runId: "88", runAttempt: "2", jobId: "104", sourceSha: PRODUCER, jobs: { candidate: "101", win32: "102", darwin: "103", qualify: "104" } }, artifact: { id: "20", digest: `sha256:${hash(zip.supplement)}`, proofSha256: "0".repeat(64) }, policy: { id: "runtime-workloads/v1", sourceSha: PRODUCER, validatorSourceSha: PRODUCER } };
  const budget = JSON.parse(fs.readFileSync(new URL(`../../crates/${crate}/runtime-budget.json`, import.meta.url), "utf8"));
  budget.status = { api: "status()", file: "src/lib.rs", exposes: ["residentMemoryBytes"] }; budget.measurements = [];
  if (budget.profiles) for (const profile of Object.values(budget.profiles)) profile.measurements = [];
  const data = shell ? {
    "Cargo.toml": '[workspace]\nmembers = ["crates/rightkit-tauri-shell"]\n',
    "crates/rightkit-tauri-shell/Cargo.toml": '[package]\nname = "rightkit-tauri-shell"\nversion = "0.5.0"\nedition = "2021"\n',
    "crates/rightkit-tauri-shell/src/lib.rs": 'pub fn status() {}\n', "crates/rightkit-tauri-shell/src/watchdog.rs": '// watchdog fixture\n',
    "crates/rightkit-tauri-shell/src/host/mod.rs": '// host fixture\n', "crates/rightkit-tauri-shell/src/host/policy.rs": '// policy fixture\n',
    "crates/rightkit-tauri-shell/src/logs_commands.rs": '// logs fixture\n',
    "crates/rightkit-tauri-shell/README.md": '## Runtime budget\n\nfixture\n',
    "crates/rightkit-tauri-shell/runtime-budget.json": JSON.stringify(budget),
    "crates/rightkit-tauri-shell/benches/workloads.json": JSON.stringify(supportedPrescription(crate)),
    "crates/rightkit-tauri-shell/benches/budget_native.rs": '// native fixture\n',
  } : daemon ? {
    "Cargo.toml": '[workspace]\nmembers = ["crates/rightkit-daemon"]\n',
    "crates/rightkit-daemon/Cargo.toml": '[package]\nname = "rightkit-daemon"\nversion = "0.4.0"\nedition = "2021"\n',
    "crates/rightkit-daemon/src/lib.rs": 'pub fn status() {}\n', "crates/rightkit-daemon/src/server.rs": '// server fixture\n',
    "crates/rightkit-daemon/src/remote/http.rs": '// remote fixture\n', "crates/rightkit-daemon/src/loopback/mod.rs": '// loopback fixture\n',
    "crates/rightkit-daemon/README.md": '## Runtime budget\n\nfixture\n',
    "crates/rightkit-daemon/runtime-budget.json": JSON.stringify(budget),
    "crates/rightkit-daemon/benches/workloads.json": JSON.stringify(supportedPrescription(crate)),
    "crates/rightkit-daemon/benches/budget.rs": '// local fixture\n', "crates/rightkit-daemon/benches/remote_budget.rs": '// remote fixture\n',
  } : {
    "Cargo.toml": '[workspace]\nmembers = ["crates/rightkit-logs"]\n',
    "crates/rightkit-logs/Cargo.toml": '[package]\nname = "rightkit-logs"\nversion = "0.3.4"\nedition = "2021"\n',
    "crates/rightkit-logs/src/lib.rs": 'pub fn status() {}\n', "crates/rightkit-logs/src/detached.rs": 'pub struct DetachedLog;\n',
    "crates/rightkit-logs/README.md": '## Runtime budget\n\nfixture\n',
    "crates/rightkit-logs/runtime-budget.json": JSON.stringify(budget),
    "crates/rightkit-logs/benches/workloads.json": JSON.stringify(supportedPrescription(CRATE)),
    "crates/rightkit-logs/benches/budget.rs": '// tracing fixture\n', "crates/rightkit-logs/benches/budget_detached.rs": '// detached fixture\n',
  };
  const readData = (name) => { assert.ok(Object.hasOwn(data, name), `missing source ${name}`); return Buffer.from(data[name]); };
  const source = { source: { sha: SOURCE }, root: path.join(temp, "candidate"), readData };
  const bindings = Object.fromEntries(Object.keys(data).filter((name) => name !== `crates/${crate}/runtime-budget.json` && name !== `crates/${crate}/README.md`).map((name) => [name, hash(readData(name))]));
  const context = { repository: REPOSITORY, runId: "77", runAttempt: "1", sourceSha: SOURCE, artifactId: 10, artifactDigest: candidate.artifacts.packed.digest, harnessSha: PRODUCER, crate };
  const files = { packed: { "manifest.json": rawManifest, [unit.file]: archive, [unit.metaFile]: metadata }, supplement: {} };
  const proof = { schema: 2, ...context, manifestSha256: candidate.manifestSha256, workloads: workloadsFor(crate).map((w) => w.id), proofFiles: {}, records: [], verdict: { ok: true } };
  const envelopes = {};
  for (const host of ["win32", "darwin"]) {
    const envelope = { schema: 2, workloads: workloadsFor(crate).map((w) => {
      const scale = scaleOf(w);
      const raw = Buffer.from(`RESULT budget crate=${crate} version=${version} resident_bytes=100 idle_cpu_percent=0 wakeups_per_s=0 disk_ops_per_s=${host === "darwin" ? "unavailable" : "0"} scale_count=${scale.count} scale_unit=${scale.unit}${shell ? " bare_wakeups_per_s=0 wakeups_delta_per_s=0" : ""}\n`);
      const command = `cargo ${w.args.join(" ")}`;
      const record = { host: { os: host, arch: host === "darwin" ? "arm64" : "x64" }, crateVersion: unit.version, commit: SOURCE, date: "2026-10-20", scope: "synthetic", dataScale: { description: w.description, unit: scale.unit, count: scale.count }, command, observed: { residentMemoryBytes: 100, idleCpuPercent: 0, wakeupsPerSecond: 0, diskOpsPerSecond: host === "darwin" ? null : 0 }, ...(shell ? { baseline: { wakeupsPerSecond: 0 }, delta: { wakeupsPerSecond: 0 } } : {}), evidence: { file: `runtime-budget.evidence/${host}-${w.id}.txt`, sha256: hash(raw) } };
      for (const suffix of ["bench", "evidence"]) files.supplement[`${host}/${w.id}-${suffix}.txt`] = raw;
      return { schema: 2, workload: w.id, intervalMs: w.intervalMs ?? null, crate: unit.name, version: unit.version, ...context, qualificationRunId: "88", qualificationRunAttempt: "2", manifestSha256: candidate.manifestSha256, bindings, toolchain: { rustc: "rustc fixture", cargo: "cargo fixture" }, host: record.host, command, envNames: [...w.env], rawFile: `${w.id}-bench.txt`, rawSha256: hash(raw), exitCode: 0, signal: null, problems: [], record };
    }) };
    envelopes[host] = envelope;
  }
  const refresh = () => {
    proof.records = [];
    for (const [host, envelope] of Object.entries(envelopes)) { files.supplement[`${host}/qualification.json`] = Buffer.from(JSON.stringify(envelope)); proof.records.push(...envelope.workloads.map((r) => r.record)); }
    proof.proofFiles = Object.fromEntries(Object.entries(files.supplement).filter(([name]) => name !== "qualification.json").map(([name, bytes]) => [name, { sha256: hash(bytes), size: bytes.length }]));
    const raw = Buffer.from(JSON.stringify(proof)); files.supplement["qualification.json"] = raw; pin.artifact.proofSha256 = hash(raw);
  };
  refresh();
  const repository = { id: 99, full_name: REPOSITORY, default_branch: "main" };
  const sourceWorkflow = { id: 5, path: ".github/workflows/publish.yml", state: "active" }, workflow = { id: 6, path: ".github/workflows/qualify-runtime.yml", state: "active" };
  const run = (id, attempt, head, flow) => ({ id, run_attempt: attempt, repository, head_repository: repository, workflow_id: flow.id, path: flow.path, event: "workflow_dispatch", head_branch: "main", head_sha: head, status: "completed", conclusion: "success" });
  const job = (id, name, runId, attempt, head) => ({ id, name, run_id: runId, run_attempt: attempt, head_sha: head, status: "completed", conclusion: "success" });
  const artifact = (id, name, runId, head, digest) => ({ id, name, expired: false, digest, workflow_run: { id: runId, head_sha: head, head_branch: "main", repository_id: 99, head_repository_id: 99 } });
  const helperMutations = new Map();
  const fetchImpl = async (url) => {
    const route = String(url).slice(`https://api.github.com/repos/${REPOSITORY}`.length).replace(/^\//, "");
    let doc;
    if (!route) doc = repository;
    else if (route === "actions/workflows/publish.yml") doc = sourceWorkflow;
    else if (route === "actions/workflows/qualify-runtime.yml") doc = workflow;
    else if (route === "actions/runs/77/attempts/1") doc = run(77, 1, SOURCE, sourceWorkflow);
    else if (route === "actions/runs/88/attempts/2") doc = run(88, 2, PRODUCER, workflow);
    else if (route === "actions/runs/77/attempts/1/jobs?per_page=100") { const names = ["guard", "plan", "pack", "verify (xcode-27, 26.8.1)", "verify (windows-2025, 26.8.1)"]; doc = { total_count: names.length, jobs: names.map((name, i) => job(i+1, name, 77, 1, SOURCE)) }; }
    else if (route === "actions/runs/88/attempts/2/jobs?per_page=100") doc = { total_count: 4, jobs: ["candidate", "measure (windows-2025, win32)", "measure (xcode-27, darwin)", "qualify"].map((name, i) => job(i+101, name, 88, 2, PRODUCER)) };
    else if (route.startsWith("compare/")) doc = { status: "ahead", merge_base_commit: { sha: route.includes(SOURCE) ? SOURCE : PRODUCER } };
    else if (route === "actions/artifacts/10") doc = artifact(10, "publish-packed", 77, SOURCE, candidate.artifacts.packed.digest);
    else if (route === "actions/artifacts/20") doc = artifact(20, "runtime-qualified", 88, PRODUCER, pin.artifact.digest);
    else if (route === "actions/artifacts/10/zip" || route === "actions/artifacts/20/zip") return { ok: true, arrayBuffer: async () => zip[route.includes("/10/") ? "packed" : "supplement"] };
    else if (route.startsWith("contents/")) {
      const name = route.slice("contents/".length).split("?ref=")[0];
      const bytes = helperMutations.get(name) ?? fs.readFileSync(new URL(`../../${name}`, import.meta.url));
      doc = { encoding: "base64", content: bytes.toString("base64"), sha: sha("sha1", Buffer.concat([Buffer.from(`blob ${bytes.length}\0`), bytes])) };
    } else assert.fail(`unexpected route ${route}`);
    return { ok: true, json: async () => structuredClone(doc) };
  };
  const spawn = (exe, args) => {
    if (exe === "tar") {
      const member = args[2].replace(`${crate}-${version}/`, "").replace("Cargo.toml.orig", "Cargo.toml");
      return { status: 0, stdout: readData(`crates/${crate}/${member}`) };
    }
    assert.equal(exe, "unzip", "admission never executes candidate code");
    const kind = path.basename(args[1], ".zip");
    return { status: 0, stdout: args[0] === "-Z1" ? Object.keys(files[kind]).join("\n") : files[kind][args[2]] };
  };
  const env = { GITHUB_ACTIONS: "true", GITHUB_REPOSITORY: REPOSITORY, GITHUB_REF: "refs/heads/main", GITHUB_SHA: VALIDATOR, GITHUB_WORKFLOW_REF: `${REPOSITORY}/.github/workflows/publish-reviewed.yml@refs/heads/main`, GITHUB_TOKEN: "read-fixture", RUNNER_TEMP: temp };
  const verify = () => verifyRuntimeSupplement({ candidate, pin, manifest, manifestSha256: candidate.manifestSha256, source, env, fetchImpl, spawn });
  return { verify, candidate, unit, proof, pin, envelopes, files, refresh, helperMutations, budget, data, source };
}
test("fully authenticated runtime supplement admits exactly four raw owner-validated records", async (t) => {
  const f = runtimeFixture(t), handle = await f.verify();
  const records = authenticatedRuntimeProof(handle, f.candidate, f.unit);
  assert.equal(records.length, 4);
  assert.deepEqual(records.map((r) => `${r.host.os}/${r.command.split(" ").at(-1)}`), ["win32/budget", "win32/budget_detached", "darwin/budget", "darwin/budget_detached"]);
});
test("authenticated hashes cannot bless fabricated RESULT values or invalid owner metadata", async (t) => {
  for (const mutation of [
    (f) => { f.envelopes.win32.workloads[0].record.observed.residentMemoryBytes = 101; },
    (f) => { f.envelopes.win32.workloads[0].record.date = "not-a-date"; },
    (f) => { f.envelopes.win32.workloads[0].record.dataScale.description = ""; },
    (f) => { f.files.supplement["win32/tracing-bench.txt"] = Buffer.from("no actual RESULT\n"); const r=f.envelopes.win32.workloads[0]; r.rawSha256=hash(f.files.supplement["win32/tracing-bench.txt"]); r.record.evidence.sha256=r.rawSha256; f.files.supplement["win32/tracing-evidence.txt"]=f.files.supplement["win32/tracing-bench.txt"]; },
    (f) => { const b=JSON.parse(f.data["crates/rightkit-logs/runtime-budget.json"]); b.ceilings.wakeupsPerSecond=1000; f.data["crates/rightkit-logs/runtime-budget.json"]=JSON.stringify(b); },
    (f) => { f.helperMutations.set("packages/release/runtime-budgets.mjs", Buffer.from("// changed owner semantics")); },
    (f) => { f.helperMutations.set("packages/release/crate-graph.mjs", Buffer.from("// changed dependency graph")); },
    (f) => { f.helperMutations.set("scripts/ci/registry-consumer.mjs", Buffer.from("// changed archive tool")); },
    (f) => { f.helperMutations.set("scripts/ci/consumer-recipes.mjs", Buffer.from("// changed transitive harness helper")); },
  ]) { const f = runtimeFixture(t); mutation(f); f.refresh(); await assert.rejects(f.verify(), /runtime supplement refused/); }
});
test("runtime proof crate must be a sealed candidate unit with a valid name and a present, pinned prescription", async (t) => {
  for (const mutation of [
    (f) => { f.proof.crate = "rightkit-daemon"; },                       // not a candidate unit
    (f) => { f.proof.crate = "rightkit-Logs"; },                         // fails the name grammar
    (f) => { f.proof.crate = "rightkit-logs/../rightkit-logs"; },        // path-bearing name
    (f) => { delete f.proof.crate; },                                    // unnamed proof
    (f) => { delete f.data["crates/rightkit-logs/benches/workloads.json"]; }, // missing workloads.json
    (f) => { // workloads.json naming a bench that does not exist
      const prescription = JSON.parse(f.data["crates/rightkit-logs/benches/workloads.json"]);
      prescription.workloads[1].bench = "budget_absent"; prescription.workloads[1].args[9] = "budget_absent";
      f.data["crates/rightkit-logs/benches/workloads.json"] = JSON.stringify(prescription);
    },
  ]) { const f = runtimeFixture(t); mutation(f); f.refresh(); await assert.rejects(f.verify(), /runtime supplement refused/); }
});

test("trusted owner gate rejects negative mismatched bare baseline floor even when raw agrees", (t) => {
  const f = runtimeFixture(t), record = structuredClone(f.envelopes.win32.workloads[0].record);
  record.observed.wakeupsPerSecond = 0; record.baseline = { wakeupsPerSecond: 10 }; record.delta = { wakeupsPerSecond: -10 };
  const raw = Buffer.from("RESULT budget crate=rightkit-logs version=0.3.4 resident_bytes=100 idle_cpu_percent=0 wakeups_per_s=0 disk_ops_per_s=0 scale_count=200000 scale_unit=lines bare_wakeups_per_s=10 wakeups_delta_per_s=-10\n");
  record.evidence.sha256 = hash(raw);
  const budget = { ...f.budget, ceilingBasis: { wakeupsPerSecond: "delta-over-bare" }, deltaCeilings: { wakeupsPerSecond: 1 } };
  assert.throws(() => validateRuntimeRecords({ source: f.source, sourceSha: SOURCE, unit: f.unit, budget, records: [record], rawByEvidence: new Map([[record.evidence.file, raw]]) }), /bare baseline/);
});
test("frozen budget & trusted helper closure include consumed parsing/graph semantics", () => {
  const budget = { ceilings: { wakeupsPerSecond: 1 }, requiredScale: { unit: "lines", minCount: 200000 } };
  validateFrozenRuntimeBudget(structuredClone(budget), budget);
  for (const key of ["ceilings", "requiredScale", "ceilingBasis", "deltaCeilings"]) assert.throws(() => validateFrozenRuntimeBudget({ ...budget, [key]: {} }, budget), /frozen/);
  const paths = trustedRuntimeHelperPaths();
  for (const name of ["packages/release/runtime-budgets.mjs", "packages/release/crate-graph.mjs", "packages/release/publication-gate.mjs", "scripts/ci/runtime-qualify.mjs", "scripts/ci/registry-consumer.mjs", "scripts/ci/consumer-recipes.mjs", "scripts/ci/upload.mjs", "pnpm-lock.yaml"]) assert.ok(paths.includes(name), `missing trusted helper ${name}`);
  assert.ok(paths.includes("scripts/ci/npm-recipe-consumer.mjs"));
  assert.ok(!paths.includes("scripts/ci/provenance.mjs"), "generated scratch import is not a source dependency");
});

function mockTrustedHelperSources(t, sources) {
  const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
  t.mock.method(fs, "readFileSync", (absolute) => {
    const name = path.relative(root, absolute).split(path.sep).join("/");
    if (!sources.has(name)) throw Object.assign(new Error(`missing helper ${name}`), { code: "ENOENT" });
    return sources.get(name);
  });
}

test("trusted helper closure parses real imports & ignores generated source, strings, regexes & comments", (t) => {
  const sources = new Map([
    ["packages/release/runtime-budgets.mjs", ""],
    ["scripts/ci/runtime-qualify.mjs", [
      "// import './line-comment.mjs';",
      "/* export { x } from './block-comment.mjs'; */",
      "const string = \"import './string.mjs'\";",
      "const regex = /import '.\\/regex.mjs'/;",
      "const generated = `import value from './provenance.mjs'; ${import('./interpolation.mjs')}`;",
      "const nested = `outer ${`inner ${import('./nested-interpolation.mjs')}`}`;",
      "import './side-effect.mjs';",
      "import value from './static.mjs';",
      "export { value } from './named-export.mjs';",
      "export * from './star-export.mjs';",
      "const loaded = import /* comment */ ('./dynamic.mjs');",
      String.raw`import './escaped\u002dmjs.mjs';`,
      "const ownUrl = import.meta.url;",
      "import 'node:fs';",
    ].join("\n")],
  ]);
  const expected = ["interpolation", "nested-interpolation", "side-effect", "static", "named-export", "star-export", "dynamic", "escaped-mjs"].map((name) => `scripts/ci/${name}.mjs`);
  for (const name of expected) sources.set(name, "");
  // Transitive traversal & cycles remain intact.
  sources.set("scripts/ci/static.mjs", "export * from './transitive.mjs';");
  sources.set("scripts/ci/transitive.mjs", "import './static.mjs';");
  mockTrustedHelperSources(t, sources);
  assert.deepEqual(trustedRuntimeHelperPaths(), [...sources.keys(), "pnpm-lock.yaml", "package.json", "packages/release/package.json"].sort());
});

test("trusted helper closure refuses computed imports instead of omitting dependencies", (t) => {
  const sources = new Map([["packages/release/runtime-budgets.mjs", ""], ["scripts/ci/runtime-qualify.mjs", ""]]);
  mockTrustedHelperSources(t, sources);
  for (const expression of ["name", "'./' + name", "`./${name}.mjs`", "new URL('./helper.mjs', import.meta.url)"]) {
    sources.set("scripts/ci/runtime-qualify.mjs", `import(${expression});`);
    assert.throws(() => trustedRuntimeHelperPaths(), /computed helper import cannot prove closure: scripts\/ci\/runtime-qualify.mjs/);
  }
});

test("trusted computed helper descriptor binds whole source, expression, count & both recursive targets", (t) => {
  const source = fs.readFileSync(new URL("../../packages/release/local-publish.mjs", import.meta.url), "utf8");
  const name = "packages/release/local-publish.mjs";
  const expression = 'pathToFileURL(path.join(workspaceRoot, "scripts", "ci", file)).href';
  const sources = new Map([
    ["packages/release/runtime-budgets.mjs", "import './local-publish.mjs';"],
    ["scripts/ci/runtime-qualify.mjs", ""],
    [name, source],
    ["scripts/ci/test-results.mjs", "import './descriptor-transitive.mjs';"],
    ["scripts/ci/upload.mjs", ""],
    ["scripts/ci/descriptor-transitive.mjs", ""],
  ]);
  for (const helper of ["background-process", "managed-cargo", "execution-policy", "publication-gate", "publish-cargo", "cargo-registry-readback", "crate-graph", "rightkit-sync", "swap-helper-bundle", "host-upload-refusal"]) sources.set(`packages/release/${helper}.mjs`, "");
  mockTrustedHelperSources(t, sources);
  assert.deepEqual(trustedRuntimeHelperPaths(), [...sources.keys(), "pnpm-lock.yaml", "package.json", "packages/release/package.json"].sort());
  for (const changed of [
    `${source}\n// descriptor source changed\n`,
    source.replace(expression, 'pathToFileURL(path.join(workspaceRoot, "scripts", "ci", "evil.mjs")).href'),
    source.replace(`import(${expression})`, "Promise.resolve({})"),
    `${source}\nconst extra = () => import(${expression});\n`,
    source.replace('load("upload.mjs")', 'load("upload.mjs"), load("evil.mjs")'),
  ]) {
    sources.set(name, changed);
    assert.throws(() => trustedRuntimeHelperPaths(), /computed helper source differs from trusted descriptor: packages\/release\/local-publish.mjs/);
  }
});

test("trusted helper closure still refuses missing real source dependencies & escaping imports", (t) => {
  const sources = new Map([["packages/release/runtime-budgets.mjs", ""], ["scripts/ci/runtime-qualify.mjs", "import './missing.mjs';"]]);
  mockTrustedHelperSources(t, sources);
  assert.throws(() => trustedRuntimeHelperPaths(), { code: "ENOENT" });
  sources.set("scripts/ci/runtime-qualify.mjs", "import '../../../outside.mjs';");
  assert.throws(() => trustedRuntimeHelperPaths(), /helper import escapes trusted source/);
});

// rightkit-daemon: three workloads per host; the remote workload is held to the remote profile, not the top-level ceilings.
const DAEMON = "rightkit-daemon";
function rewriteRaw(f, host, id, { resident, count }) {
  const row = f.envelopes[host].workloads.find((w) => w.workload === id);
  let text = f.files.supplement[`${host}/${id}-bench.txt`].toString("utf8");
  if (resident !== undefined) text = text.replace("resident_bytes=100", `resident_bytes=${resident}`);
  if (count !== undefined) text = text.replace(/scale_count=\d+/, `scale_count=${count}`);
  const raw = Buffer.from(text);
  for (const suffix of ["bench", "evidence"]) f.files.supplement[`${host}/${id}-${suffix}.txt`] = raw;
  row.rawSha256 = row.record.evidence.sha256 = hash(raw);
  if (resident !== undefined) row.record.observed.residentMemoryBytes = resident;
  if (count !== undefined) row.record.dataScale.count = count;
}
test("daemon runtime supplement admits six raw records and holds remote to its pinned profile", async (t) => {
  const f = runtimeFixture(t, DAEMON), handle = await f.verify();
  const records = authenticatedRuntimeProof(handle, f.candidate, f.unit);
  assert.equal(records.length, 6);
  assert.deepEqual(records.slice(0, 3).map((r) => r.dataScale.unit), ["connections", "submissions", "connections"]);
  // 50 MB exceeds the 23 MB top-level ceiling but is inside the 95 MiB remote profile ceiling.
  const remote = runtimeFixture(t, DAEMON);
  rewriteRaw(remote, "win32", "remote", { resident: 50_000_000 }); remote.refresh();
  await remote.verify();
  const local = runtimeFixture(t, DAEMON);
  rewriteRaw(local, "win32", "local", { resident: 50_000_000 }); local.refresh();
  await assert.rejects(local.verify(), /refused|exceeds/);
  const loopback = runtimeFixture(t, DAEMON);
  rewriteRaw(loopback, "darwin", "remote-loopback", { resident: 50_000_000 }); loopback.refresh();
  await assert.rejects(loopback.verify(), /refused|exceeds/);
});
test("daemon runtime supplement refuses loosened profile, short scale, altered identity & altered prescription", async (t) => {
  const edit = (f, change) => { const b = JSON.parse(f.data["crates/rightkit-daemon/runtime-budget.json"]); change(b); f.data["crates/rightkit-daemon/runtime-budget.json"] = JSON.stringify(b); };
  const prescribe = (f, change) => { const p = JSON.parse(f.data["crates/rightkit-daemon/benches/workloads.json"]); change(p); f.data["crates/rightkit-daemon/benches/workloads.json"] = JSON.stringify(p); };
  for (const [mutation, expected] of [
    [(f) => rewriteRaw(f, "win32", "remote", { resident: 100_000_000 }), /exceeds or cannot prove residentMemoryBytes/],  // above the profile ceiling
    [(f) => rewriteRaw(f, "darwin", "remote", { count: 9999 }), /below required scale/],                                    // below the profile scale
    [(f) => rewriteRaw(f, "darwin", "local", { count: 63 }), /below required scale/],                                       // below the top-level scale
    [(f) => edit(f, (b) => { b.profiles.remote.ceilings.residentMemoryBytes = 900_000_000; }), /frozen profiles/],
    [(f) => edit(f, (b) => { b.profiles.remote.requiredScale.minCount = 1; }), /frozen profiles/],
    [(f) => edit(f, (b) => { b.profiles.remote.features = ["link"]; }), /frozen profiles/],
    [(f) => edit(f, (b) => { b.ceilings.residentMemoryBytes = 900_000_000; }), /frozen ceilings/],
    [(f) => { f.envelopes.win32.workloads[1].envNames = []; }, /identity.status mismatch/],                                // env names differ from the trusted table
    [(f) => { f.envelopes.win32.workloads[1].envNames.push("RIGHTKIT_EXTRA"); }, /identity.status mismatch/],
    [(f) => prescribe(f, (p) => p.workloads[2].args.push("1")), /runtime supplement refused/],
    [(f) => prescribe(f, (p) => { p.workloads[1].args = p.workloads[1].args.filter((a) => a !== "--locked"); }), /runtime supplement refused/],
    [(f) => prescribe(f, (p) => { p.workloads[0].env = ["RIGHTKIT_RUNTIME_SCRATCH_ROOT"]; }), /runtime supplement refused/],
    [(f) => f.helperMutations.set("scripts/ci/runtime-workload-env.mjs", Buffer.from("// changed environment provider")), /runtime supplement refused/],
  ]) { const f = runtimeFixture(t, DAEMON); mutation(f); f.refresh(); await assert.rejects(f.verify(), expected); }
});
test("frozen view covers each profile's ceilings, scale & features but not its measurements", () => {
  const budget = { ceilings: { wakeupsPerSecond: 1 }, requiredScale: { unit: "connections", minCount: 64 }, profiles: { remote: { features: ["remote"], ceilings: { residentMemoryBytes: 10 }, requiredScale: { unit: "submissions", minCount: 10000 }, measurements: [] } } };
  validateFrozenRuntimeBudget({ ...budget, profiles: { remote: { ...budget.profiles.remote, measurements: [{ any: "history" }], measurementsPending: "note" } } }, budget);
  for (const change of [{ ceilings: { residentMemoryBytes: 11 } }, { requiredScale: { unit: "submissions", minCount: 1 } }, { features: [] }, { ceilingBasis: { wakeupsPerSecond: "delta-over-bare" } }]) assert.throws(() => validateFrozenRuntimeBudget({ ...budget, profiles: { remote: { ...budget.profiles.remote, ...change } } }, budget), /frozen/);
  assert.throws(() => validateFrozenRuntimeBudget({ ...budget, profiles: undefined }, budget), /frozen/);
  assert.throws(() => validateFrozenRuntimeBudget({ ...budget, profiles: { ...budget.profiles, extra: { features: ["x"] } } }, budget), /frozen/);
});

// rightkit-tauri-shell: four native rows per host, wake-ups gated as a delta over the bare baseline.
const SHELL = "rightkit-tauri-shell";
function rewriteDelta(f, host, id, { observed, bare, delta, drop = false }) {
  const row = f.envelopes[host].workloads.find((w) => w.workload === id);
  let text = f.files.supplement[`${host}/${id}-bench.txt`].toString("utf8");
  text = text.replace("wakeups_per_s=0", `wakeups_per_s=${observed}`).replace("bare_wakeups_per_s=0", `bare_wakeups_per_s=${bare}`).replace("wakeups_delta_per_s=0", `wakeups_delta_per_s=${delta}`);
  const raw = Buffer.from(text);
  for (const suffix of ["bench", "evidence"]) f.files.supplement[`${host}/${id}-${suffix}.txt`] = raw;
  row.rawSha256 = row.record.evidence.sha256 = hash(raw);
  row.record.observed.wakeupsPerSecond = observed;
  row.record.baseline = { wakeupsPerSecond: bare }; row.record.delta = { wakeupsPerSecond: delta };
  if (drop) { delete row.record.baseline; delete row.record.delta; }
}
test("tauri-shell runtime supplement admits eight delta-gated native records", async (t) => {
  const f = runtimeFixture(t, SHELL), handle = await f.verify();
  const records = authenticatedRuntimeProof(handle, f.candidate, f.unit);
  assert.equal(records.length, 8);
  assert.ok(records.every((r) => r.command.includes("--no-default-features") && r.command.endsWith("--bench budget_native") && r.dataScale.unit === "watchdogs" && r.baseline && r.delta));
  assert.deepEqual(records.slice(0, 4).map((r) => r.command.split(" --features ")[1].split(" ")[0]), ["wv2-0_38", "wv2-0_39", "wv2-0_38,host-services,logs,crash,content-devtools", "wv2-0_39,host-services,logs,crash,content-devtools"]);
  // A whole-process wake-up rate that would fail the 10.2/s ceiling passes only as a delta of at most 4 over the bare baseline.
  const within = runtimeFixture(t, SHELL);
  rewriteDelta(within, "darwin", "native-host-wv2-0_39", { observed: 9, bare: 6, delta: 3 }); within.refresh();
  await within.verify();
});
test("tauri-shell runtime supplement refuses bad delta, scale, env & altered prescription", async (t) => {
  const prescribe = (f, change) => { const p = JSON.parse(f.data["crates/rightkit-tauri-shell/benches/workloads.json"]); change(p); f.data["crates/rightkit-tauri-shell/benches/workloads.json"] = JSON.stringify(p); };
  for (const [mutation, expected] of [
    [(f) => rewriteDelta(f, "win32", "native-shell-wv2-0_38", { observed: 6, bare: 1, delta: 5 }), /wakeupsPerSecond/],           // delta above 4
    [(f) => rewriteDelta(f, "darwin", "native-host-wv2-0_38", { observed: 3, bare: 1, delta: 0.5 }), /baseline.delta/],             // reported delta disagrees
    [(f) => rewriteDelta(f, "darwin", "native-shell-wv2-0_39", { observed: 0, bare: 0, delta: 0, drop: true }), /baseline.delta/],  // no baseline
    [(f) => { f.envelopes.win32.workloads[0].record.dataScale.unit = "connections"; }, /below required scale|differs/],
    [(f) => { f.envelopes.win32.workloads[2].envNames = ["RIGHTKIT_RUNTIME_SCRATCH_ROOT"]; }, /identity.status mismatch/],
    [(f) => prescribe(f, (p) => { p.workloads[0].args = p.workloads[0].args.filter((a) => a !== "--no-default-features"); }), /runtime supplement refused/],
    [(f) => prescribe(f, (p) => { p.workloads[2].args = p.workloads[2].args.map((a) => a.replace(",crash", "")); }), /runtime supplement refused/],
    [(f) => prescribe(f, (p) => { p.workloads[3].intervalMs = 2000; }), /runtime supplement refused/],
    [(f) => prescribe(f, (p) => { p.workloads = p.workloads.slice(0, 3); }), /runtime supplement refused/],
    [(f) => prescribe(f, (p) => { delete p.coverage.engine; }), /runtime supplement refused/],
    [(f) => prescribe(f, (p) => { p.coverage.crash = "weaker"; }), /runtime supplement refused/],
  ]) { const f = runtimeFixture(t, SHELL); mutation(f); f.refresh(); await assert.rejects(f.verify(), expected); }
});

// Admission policy: one authenticated handle per crate; workloads always come from the trusted coverage table.
test("runtime admission qualifies each trusted crate by its own handle and trusted workload ids", async (t) => {
  const crates = { [CRATE]: ["tracing", "detached"], [DAEMON]: ["local", "remote", "remote-loopback"], [SHELL]: ["native-shell-wv2-0_38", "native-shell-wv2-0_39", "native-host-wv2-0_38", "native-host-wv2-0_39"] };
  const made = {};
  for (const crate of Object.keys(crates)) { const f = runtimeFixture(t, crate); made[crate] = { f, handle: await f.verify() }; }
  for (const [crate, ids] of Object.entries(crates)) {
    const { f, handle } = made[crate], others = Object.entries(made).filter(([name]) => name !== crate).map(([, value]) => value.handle);
    const policy = (extra) => runtimeFeatureProofPolicy({ crate, meta: {}, hasFile: true, candidate: f.candidate, unit: f.unit, ...extra });
    for (const extra of [{ runtimeProof: handle }, { runtimeProofs: [handle] }, { runtimeProofs: [...others, handle] }, { runtimeProof: others[0], runtimeProofs: [handle, others[1]] }]) {
      const result = policy(extra);
      assert.equal(result.status, "qualified");
      assert.deepEqual(result.proof, { candidateId: f.candidate.id, proofSha256: handle.proofSha256, workloads: ids });
      assert.deepEqual(result.proof.workloads, workloadsFor(crate).map((w) => w.id));
    }
    // Another crate's handle, no handle, or a claimed handle never qualifies.
    for (const extra of [{ runtimeProofs: others }, {}, { runtimeProof: { candidateId: f.candidate.id, proofSha256: handle.proofSha256 } }]) assert.equal(policy(extra).status, "HOLD");
  }
  // A crate without trusted coverage cannot be qualified even with an authenticated handle for another crate.
  const { f, handle } = made[CRATE];
  assert.equal(runtimeFeatureProofPolicy({ crate: "rightkit-job-queue", meta: {}, hasFile: true, runtimeProofs: [handle], candidate: f.candidate, unit: { ...f.unit, name: "rightkit-job-queue" } }).status, "HOLD");
});
