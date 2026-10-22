// Local runtime proof: what admission accepts from the pinned receipts of a LOCAL candidate and what it refuses.
// The fixture is a rightkit-logs candidate whose two native receipts each carry three repeats of both trusted workloads.
import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { gzipSync } from "node:zlib";
import { fileURLToPath } from "node:url";
import { checkLocalReceipts, localRuntimeShapeProblems, localRuntimeProofSha256, canonicalJson, RUNTIME_PROOF_WORKLOADS, LOCAL_RUNTIME_MIN_REPEATS, LOCAL_RUNTIME_MAX_REPEATS } from "./upload.mjs";
import { localAssetName, localTag, RUNTIME_PROOF_WORKLOADS as REEXPORTED } from "./reviewed-candidate.mjs";
import { workloadsFor, supportedPrescription } from "./runtime-qualify.mjs";
import { verifyLocalRuntimeProof, authenticatedRuntimeProof, trustedRuntimeHelperPaths } from "./runtime-supplement.mjs";
import { localRequiredScanIds } from "./scan-receipt.mjs";
import { localTestPlan } from "../../packages/release/local-publish.mjs";
import { runtimeFeatureProofPolicy } from "./semantic-admission.mjs";
import { localAdmission } from "./local-admission.mjs";
import { buildLocalScanRecord, SCAN_TOOL_PINS } from "./scan-receipt.mjs";
import { WINDOW_POLICY, WORKLOAD_MIN_WINDOW_MS, minWindowMsFor, windowProblems, worstIndex } from "./local-runtime-proof.mjs";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const CRATE = "rightkit-logs", VERSION = "0.3.4", SOURCE = "a".repeat(40), TREE = "c".repeat(40), ID = "local-logs-034-aaaaaa";
const hash = (bytes) => crypto.createHash("sha256").update(bytes).digest("hex");
const clone = (x) => structuredClone(x);
const T0 = 1_800_000_000_000;

function tarGz(root, files) {
  const blocks = [];
  for (const [name, data] of Object.entries(files)) {
    const bytes = Buffer.from(data), h = Buffer.alloc(512);
    h.write(`${root}/${name}`, 0, 100); h.write("0000644\0", 100); h.write("0000000\0", 108); h.write("0000000\0", 116);
    h.write(`${bytes.length.toString(8).padStart(11, "0")}\0`, 124); h.write("00000000000\0", 136); h.write("        ", 148); h.write("0", 156); h.write("ustar\0", 257); h.write("00", 263);
    let sum = 0; for (const b of h) sum += b; h.write(`${sum.toString(8).padStart(6, "0")}\0 `, 148);
    blocks.push(h, bytes, Buffer.alloc((512 - (bytes.length % 512)) % 512));
  }
  blocks.push(Buffer.alloc(1024));
  return gzipSync(Buffer.concat(blocks));
}
const health = (written, over = {}) => ({ writtenAtMs: written, sampledAtMs: written + 500, state: "READY", admissionPaused: false, inFlight: 0, queued: 0, admitting: 0, pid: 1, ...over });
/** A quiet window starting at t0: idle broker and CPU before, only the measured job during, idle again after, one job in the listing. */
export function quietWindow(t0, durationMs = 40_000) {
  const end = t0 + durationMs;
  const during = []; for (let at = t0 + 2000; at < end; at += 5000) during.push(health(at, { inFlight: 1 }));
  return {
    policy: WINDOW_POLICY.id, startedAtMs: t0, endedAtMs: end, quiet: true,
    before: { health: { ...health(t0 - 3000), sampledAtMs: t0 - 1000 }, cpu: { startedAtMs: t0 - 11_000, durationMs: 10_500, idlePercent: 98, cores: 8, loadavg1: null } },
    during: { health: during },
    after: { health: { ...health(end + 500), sampledAtMs: end + 1000 }, cpu: { startedAtMs: end + 1000, durationMs: 5500, idlePercent: 97, cores: 8, loadavg1: null } },
    jobs: { checked: true, fromMs: t0 - 11_000, toMs: end + 6500, checkedAtMs: end + 7000, ours: 1, foreign: [], atGrant: { checked: true, checkedAtMs: t0 - 200, ids: ["j-0"], live: [] }, atRelease: { checked: true, checkedAtMs: end + 7000, ids: ["j-0", "j-1"], live: [] } },
  };
}

function runtimeFixture(t) {
  const scratch = process.env.RIGHTKIT_TEST_SCRATCH ?? process.env.RUNNER_TEMP ?? os.tmpdir();
  const temp = fs.realpathSync.native(fs.mkdtempSync(path.join(scratch, "rk-localrt-")));
  t.after(() => fs.rmSync(temp, { recursive: true, force: true }));
  const budget = JSON.parse(fs.readFileSync(path.join(ROOT, "crates", CRATE, "runtime-budget.json"), "utf8"));
  budget.status = { api: "status()", file: "src/lib.rs", exposes: ["residentMemoryBytes"] }; budget.measurements = [];
  const cargoToml = `[package]\nname = "${CRATE}"\nversion = "${VERSION}"\nedition = "2021"\n`;
  const data = {
    "Cargo.toml": `[workspace]\nmembers = ["crates/${CRATE}"]\n`, "Cargo.lock": `# Cargo.lock\nversion = 3\n\n[[package]]\nname = "${CRATE}"\nversion = "${VERSION}"\n`,
    [`crates/${CRATE}/Cargo.toml`]: cargoToml, [`crates/${CRATE}/src/lib.rs`]: "pub fn status() {}\n", [`crates/${CRATE}/src/detached.rs`]: "pub struct DetachedLog;\n",
    [`crates/${CRATE}/README.md`]: "## Runtime budget\n\nfixture\n", [`crates/${CRATE}/runtime-budget.json`]: JSON.stringify(budget),
    [`crates/${CRATE}/benches/workloads.json`]: JSON.stringify(supportedPrescription(CRATE)),
    [`crates/${CRATE}/benches/budget.rs`]: "// tracing fixture\n", [`crates/${CRATE}/benches/budget_detached.rs`]: "// detached fixture\n",
  };
  const readData = (name) => { if (!Object.hasOwn(data, name)) throw new Error(`candidate missing ${name}`); return Buffer.from(data[name]); };
  // The unit's directory on disk: local admission reads Cargo.toml and the budget file's presence from source.root.
  const unitDir = path.join(temp, "source", "crates", CRATE); fs.mkdirSync(unitDir, { recursive: true });
  fs.writeFileSync(path.join(unitDir, "Cargo.toml"), cargoToml); fs.writeFileSync(path.join(unitDir, "runtime-budget.json"), data[`crates/${CRATE}/runtime-budget.json`]);
  const source = { source: { sha: SOURCE, tree: TREE }, root: path.join(temp, "source"), readData, hosts: { macos: { unitPolicies: [] }, windows: { unitPolicies: [] } } };
  const crateBytes = tarGz(`${CRATE}-${VERSION}`, { ".cargo_vcs_info.json": JSON.stringify({ git: { sha1: SOURCE }, path_in_vcs: `crates/${CRATE}` }), "Cargo.toml": cargoToml, "Cargo.toml.orig": cargoToml, "src/lib.rs": data[`crates/${CRATE}/src/lib.rs`], "src/detached.rs": data[`crates/${CRATE}/src/detached.rs`] });
  const unit = { name: CRATE, version: VERSION, dir: `crates/${CRATE}`, file: `${CRATE}-${VERSION}.crate`, sha256: hash(crateBytes), size: crateBytes.length, metaFile: `${CRATE}-${VERSION}.upload.json` };
  const manifest = { schema: 1, registry: "crates", commit: SOURCE, source: "local", candidateId: ID, units: [unit] };
  const manifestSha256 = hash(Buffer.from(JSON.stringify(manifest)));
  const names = Object.fromEntries(["packed", "windows", "macos"].map((kind) => [kind, { name: localAssetName(ID, kind), sha256: hash(Buffer.from(kind)) }]));
  const candidate = { id: ID, source: "local", sourceSha: SOURCE, registry: "crates", units: [CRATE], allow: { hostExclusion: [], noTest: [], ignored: [], missingAssets: [] }, release: { tag: localTag(ID) }, assets: names };
  const packDir = path.join(temp, "packed"); fs.mkdirSync(packDir);
  fs.writeFileSync(path.join(packDir, unit.file), crateBytes); fs.writeFileSync(path.join(packDir, unit.metaFile), JSON.stringify({ name: CRATE, vers: VERSION, deps: [] }));
  const bindings = Object.fromEntries(Object.keys(data).filter((name) => !name.endsWith("/runtime-budget.json") && !name.endsWith("/README.md")).map((name) => [name, hash(readData(name))]));
  const helperHashes = Object.fromEntries(trustedRuntimeHelperPaths().map((name) => [name, hash(fs.readFileSync(path.join(ROOT, name)))]));
  const workloads = workloadsFor(CRATE);
  // Every step admission derives from the (fake) source for this unit, recorded as passed, as a real host run records them.
  const requiredSteps = (hostOs) => {
    let gateConfig = {}; try { gateConfig = JSON.parse(readData("ci/gate.config.json").toString("utf8")); } catch { /* no gate config in the fixture */ }
    const plan = localTestPlan({ kind: "cargo", unit: CRATE, dir: `crates/${CRATE}`, config: gateConfig, manifest: null, batch: false, platform: hostOs });
    return [
      { command: `cargo fmt -p ${CRATE} --check`, ok: true },
      { command: `scanCrateForSecrets crates/${CRATE}`, ok: true },
      ...plan.steps.map((step) => ({ command: `${step.command} ${step.args.join(" ")}`, ok: true, ...(step.test === true ? { executed: 2, failed: 0 } : {}) })),
      { command: `cargo package --locked -p ${CRATE}`, ok: true },
    ];
  };
  // The record carries every scan the verifier requires for this unit from the (fake) authenticated source, plus the formatting row.
  const scanIds = [...new Set(["cargo-metadata", "cargo-deny", "cargo-audit", "actionlint", "yaml-parse", "rustfmt:2021", `clippy:${CRATE}`, ...localRequiredScanIds({ units: [CRATE], readText: (name) => readData(name).toString("utf8") })])];
  const scans = buildLocalScanRecord({
    result: { schema: "rightkit.deterministic-scans/1", ok: true, complete: true, classes: ["A", "B"], inputDigest: "f".repeat(64), deferred: [], scans: scanIds.map((id) => ({ id, class: /^(clippy|check|check-features|doc|test):/.test(id) ? "B" : "A", command: id, status: "pass" })) },
    planned: scanIds.map((id) => ({ id })), sourceCommit: SOURCE, headCommit: SOURCE, cargoLockSha256: hash(Buffer.from(data["Cargo.lock"])),
    observedTools: { "cargo-deny": `cargo-deny ${SCAN_TOOL_PINS["cargo-deny"]}`, "cargo-audit": `cargo-audit-audit ${SCAN_TOOL_PINS["cargo-audit"]}`, actionlint: SCAN_TOOL_PINS.actionlint }, units: [CRATE], files: 5,
  });
  const receipts = {};
  const OS = { windows: "win32", macos: "darwin" }, ARCH = { windows: "x64", macos: "arm64" };
  for (const [key, host] of Object.entries(OS)) {
    const broker = { name: "rustright", client: `/rustright-${key}`, sha256: hash(Buffer.from(key)) };
    const hostBase = T0 + (host === "win32" ? 0 : 5_000_000);
    const rows = workloads.map((w, wi) => {
      const runs = [0, 1, 2].map((index) => {
        const raw = `noise\nRESULT budget crate=${CRATE} version=${VERSION} resident_bytes=${100 + 10 * index} idle_cpu_percent=0 wakeups_per_s=0 disk_ops_per_s=${host === "darwin" ? "unavailable" : "0"} scale_count=200000 scale_unit=lines\n`;
        const file = `runtime-budget.evidence/${host}-${ARCH[key]}-${VERSION}-synthetic-${w.id}.txt`;
        const record = { host: { os: host, arch: ARCH[key] }, crateVersion: VERSION, commit: SOURCE, date: "2026-10-22", scope: "synthetic", dataScale: { description: w.description, unit: "lines", count: 200000 }, command: `cargo ${w.args.join(" ")}`, observed: { residentMemoryBytes: 100 + 10 * index, idleCpuPercent: 0, wakeupsPerSecond: 0, diskOpsPerSecond: host === "darwin" ? null : 0 }, evidence: { file, sha256: hash(Buffer.from(raw)) } };
        const window = quietWindow(hostBase + (wi * 3 + index) * 100_000 + 20_000, Math.max(40_000, minWindowMsFor(CRATE, w.id) + 5000));
        return { index, startedAt: new Date(window.startedAtMs).toISOString(), endedAt: new Date(window.endedAtMs).toISOString(), exitCode: 0, signal: null, rawSha256: hash(Buffer.from(raw)), raw, record, window };
      });
      const picked = runs[2];
      return { schema: 2, source: "local", workload: w.id, intervalMs: w.intervalMs ?? null, crate: CRATE, version: VERSION, repository: "bogusyogi/rightkit", runId: ID, runAttempt: "local", sourceSha: SOURCE, artifactId: "local", artifactDigest: `local:${manifestSha256}`, harnessSha: SOURCE, qualificationRunId: ID, qualificationRunAttempt: "local", manifestSha256, bindings, toolchain: { rustc: "rustc fixture", cargo: "cargo fixture" }, host: { os: host, arch: ARCH[key], release: "1", runner: "local" }, command: `cargo ${w.args.join(" ")}`, invocation: `rightkit cargo ${w.args.join(" ")}`, envNames: [...w.env], rawFile: `${w.id}-bench.txt`, rawSha256: picked.rawSha256, exitCode: 0, signal: null, problems: [], record: clone(picked.record), selected: 2, repeats: runs };
    });
    const entry = { schema: 1, kind: "local-runtime-crate", source: "local", policy: WINDOW_POLICY.id, crate: CRATE, version: VERSION, sourceSha: SOURCE, candidateId: ID, manifestSha256, archiveSha256: unit.sha256, host: { os: host, arch: ARCH[key], release: "1", model: "cpu", cores: 8 }, broker: clone(broker), harness: { sourceSha: SOURCE, helpers: { ...helperHashes } }, workloads: rows };
    const evidence = { schema: 1, kind: "cargo", name: CRATE, version: VERSION, commit: SOURCE, artifactSha256: unit.sha256, hostPolicy: `${host}-only`, hostPolicyReason: "fixture", runs: [{ host: { os: host, arch: ARCH[key] }, commit: SOURCE, features: ["default"], artifactSha256: unit.sha256, command: `cargo test -p ${CRATE}`, operator: "agent", timestamp: "t", passed: 2, failed: 0, skipped: 0, ignored: 0, steps: requiredSteps(host) }] };
    receipts[key] = { schema: 1, kind: "local-verify", host: key, hostOs: host, arch: ARCH[key], registry: "crates", candidateId: ID, commit: SOURCE, tree: TREE, manifestSha256, broker, qualifiedAt: "2026-10-22T00:00:00Z", allowHostExclusion: [], allowNoTest: [], allowIgnored: [], allowMissingAssets: [], scans: clone(scans),
      units: [{ name: CRATE, version: VERSION, file: unit.file, sha256: unit.sha256, bytesVerified: true, archiveScan: { schema: 1, scanner: "publish-cargo/scanCrateForSecrets", archiveSha256: unit.sha256, status: "pass", files: 3, violations: [], problems: [] }, status: "pass", executed: 2, ignored: 0, skipped: 0, steps: [], evidence }],
      runtime: { schema: 1, kind: "local-runtime", crates: { [CRATE]: entry } } };
  }
  const entryOf = (key) => receipts[key].runtime.crates[CRATE];
  const verify = () => verifyLocalRuntimeProof({ candidate, manifest, manifestSha256, unit, source, receipts });
  const admit = () => localAdmission({ candidate, manifest, manifestSha256, source, receipts, packDir });
  return { temp, candidate, manifest, manifestSha256, unit, source, data, receipts, entryOf, verify, admit, packDir, workloads };
}
// Rewrites one repeat's raw RESULT text everywhere it is bound (repeat, record, and the submitted row when selected).
function setRaw(f, key, id, index, text) {
  const row = f.entryOf(key).workloads.find((w) => w.workload === id), run = row.repeats[index], digest = hash(Buffer.from(text));
  run.raw = text; run.rawSha256 = digest; run.record.evidence.sha256 = digest;
  const fields = /resident_bytes=(\d+)/.exec(text); if (fields) run.record.observed.residentMemoryBytes = Number(fields[1]);
  const count = /scale_count=(\d+)/.exec(text); if (count) run.record.dataScale.count = Number(count[1]);
  if (row.selected === index) { row.rawSha256 = digest; row.record = clone(run.record); }
}
const budgetOn = (f, change) => { const b = JSON.parse(f.data[`crates/${CRATE}/runtime-budget.json`]); change(b); f.data[`crates/${CRATE}/runtime-budget.json`] = JSON.stringify(b); };

test("the runtime table in upload.mjs is the trusted table, and repeat bounds equal the window policy", () => {
  assert.deepEqual(Object.fromEntries(Object.entries(RUNTIME_PROOF_WORKLOADS).map(([name, ids]) => [name, [...ids]])), Object.fromEntries(Object.keys(RUNTIME_PROOF_WORKLOADS).map((name) => [name, workloadsFor(name).map((w) => w.id)])));
  assert.equal(REEXPORTED, RUNTIME_PROOF_WORKLOADS);
  assert.deepEqual([LOCAL_RUNTIME_MIN_REPEATS, LOCAL_RUNTIME_MAX_REPEATS], [WINDOW_POLICY.minRepeats, WINDOW_POLICY.maxRepeats]);
});

test("a local runtime receipt on both hosts validates against the frozen budgets and yields the CI qualification", async (t) => {
  const f = runtimeFixture(t), handle = await f.verify();
  assert.equal(handle.proofSha256, localRuntimeProofSha256(f.receipts, CRATE));
  const records = authenticatedRuntimeProof(handle, f.candidate, f.unit);
  assert.deepEqual(records.map((r) => `${r.host.os}/${r.command.split(" ").at(-1)}`), ["win32/budget", "win32/budget_detached", "darwin/budget", "darwin/budget_detached"]);
  assert.ok(records.every((r) => r.observed.residentMemoryBytes === 120), "the worst repeat is the submitted record");
  const policy = runtimeFeatureProofPolicy({ crate: CRATE, meta: {}, hasFile: true, runtimeProofs: [handle], candidate: f.candidate, unit: f.unit });
  assert.equal(policy.status, "qualified"); assert.deepEqual(policy.proof.workloads, ["tracing", "detached"]);
  assert.equal(runtimeFeatureProofPolicy({ crate: CRATE, meta: {}, hasFile: true, runtimeProofs: [{ ...handle }], candidate: f.candidate, unit: f.unit }).status, "HOLD", "a copied handle never qualifies");
  assert.equal(localRuntimeProofSha256(f.receipts, CRATE), localRuntimeProofSha256(JSON.parse(JSON.stringify(f.receipts)), CRATE), "the hash is stable over a JSON round trip");
  assert.equal(canonicalJson({ b: 1, a: [2, { d: 1, c: 2 }] }), '{"a":[2,{"c":2,"d":1}],"b":1}');
});

test("a loosened profile or budget, a short scale, an over-budget figure or an altered prescription is refused", async (t) => {
  const prescribe = (f, change) => { const p = JSON.parse(f.data[`crates/${CRATE}/benches/workloads.json`]); change(p); f.data[`crates/${CRATE}/benches/workloads.json`] = JSON.stringify(p); };
  for (const [name, mutate, expected] of [
    ["loosened ceiling", (f) => budgetOn(f, (b) => { b.ceilings.residentMemoryBytes = 900_000_000; }), /frozen ceilings/],
    ["loosened wakeups", (f) => budgetOn(f, (b) => { b.ceilings.wakeupsPerSecond = 1000; }), /frozen ceilings/],
    ["shorter required scale", (f) => budgetOn(f, (b) => { b.requiredScale.minCount = 1; }), /frozen requiredScale/],
    ["short scale on one repeat", (f) => setRaw(f, "macos", "tracing", 1, `RESULT budget crate=${CRATE} version=${VERSION} resident_bytes=110 idle_cpu_percent=0 wakeups_per_s=0 disk_ops_per_s=unavailable scale_count=199999 scale_unit=lines\n`), /below required scale/],
    ["over the memory ceiling on a non-submitted repeat", (f) => setRaw(f, "windows", "detached", 0, `RESULT budget crate=${CRATE} version=${VERSION} resident_bytes=900000000 idle_cpu_percent=0 wakeups_per_s=0 disk_ops_per_s=0 scale_count=200000 scale_unit=lines\n`), /exceeds or cannot prove residentMemoryBytes/],
    ["raw output without a RESULT line", (f) => setRaw(f, "windows", "tracing", 2, "no result here\n"), /refused/],
    ["record differs from raw", (f) => { f.entryOf("windows").workloads[0].repeats[1].record.observed.residentMemoryBytes = 5; }, /refused/],
    ["prescription args", (f) => prescribe(f, (p) => p.workloads[0].args.push("1")), /refused/],
    ["prescription drops --locked", (f) => prescribe(f, (p) => { p.workloads[1].args = p.workloads[1].args.filter((a) => a !== "--locked"); }), /refused/],
    ["prescription missing", (f) => { delete f.data[`crates/${CRATE}/benches/workloads.json`]; }, /refused/],
    ["env names differ from the trusted table", (f) => { f.entryOf("macos").workloads[0].envNames = ["RIGHTKIT_EXTRA"]; }, /identity.status mismatch/],
    ["command differs from the trusted command", (f) => { f.entryOf("macos").workloads[0].command = "cargo test"; }, /identity.status mismatch/],
    ["workload dropped", (f) => { f.entryOf("windows").workloads.pop(); }, /requires exactly the trusted workloads/],
    ["workload added by the receipt", (f) => { f.entryOf("windows").workloads.push({ ...clone(f.entryOf("windows").workloads[0]), workload: "invented" }); }, /refused|unknown\/duplicate|trusted workloads/],
  ]) { const f = runtimeFixture(t); mutate(f); await assert.rejects(f.verify(), expected, name); }
});

test("identity, bindings and harness: another candidate, manifest, archive, source, broker or helper is refused", async (t) => {
  for (const [name, mutate, expected] of [
    ["another candidate", (f) => { f.entryOf("windows").candidateId = "other"; }, /does not bind/],
    ["another manifest", (f) => { f.entryOf("macos").manifestSha256 = "1".repeat(64); }, /does not bind/],
    ["another archive", (f) => { f.entryOf("macos").archiveSha256 = "1".repeat(64); }, /does not bind/],
    ["another source commit", (f) => { f.entryOf("windows").sourceSha = "b".repeat(40); }, /does not bind/],
    ["another version", (f) => { f.entryOf("windows").version = "9.9.9"; }, /does not bind/],
    ["another broker", (f) => { f.entryOf("windows").broker.sha256 = "2".repeat(64); }, /broker differ/],
    ["another architecture", (f) => { f.entryOf("windows").host.arch = "arm64"; }, /host, architecture or broker differ/],
    ["host os swapped", (f) => { f.entryOf("windows").host.os = "darwin"; }, /host, architecture or broker differ/],
    ["a source binding that differs from the candidate blob", (f) => { f.data[`crates/${CRATE}/src/detached.rs`] = "pub struct Other;\n"; }, /source binding differs/],
    ["a harness helper that differs from the promotion checkout", (f) => { f.entryOf("macos").harness.helpers["scripts/ci/runtime-qualify.mjs"] = "3".repeat(64); }, /differs from trusted supported helper/],
    ["a harness helper missing", (f) => { delete f.entryOf("macos").harness.helpers["scripts/ci/runtime-qualify.mjs"]; }, /helper closure/],
    ["harness at another commit", (f) => { f.entryOf("macos").harness.sourceSha = "b".repeat(40); }, /helper closure/],
    ["window policy id", (f) => { f.entryOf("macos").policy = "local-runtime-window/0"; }, /policy mismatch/],
    ["not local", (f) => { f.entryOf("macos").source = "actions"; }, /kind, source or window policy/],
  ]) { const f = runtimeFixture(t); mutate(f); await assert.rejects(f.verify(), expected, name); }
});

test("a missing host receipt, a missing runtime block or a fabricated selection is refused", async (t) => {
  for (const [name, mutate, expected] of [
    ["no windows receipt", (f) => { delete f.receipts.windows; }, /no local runtime proof/],
    ["no macos receipt", (f) => { delete f.receipts.macos; }, /no local runtime proof/],
    ["windows receipt without runtime", (f) => { delete f.receipts.windows.runtime; }, /no local runtime proof/],
    ["runtime block for another crate", (f) => { f.receipts.macos.runtime.crates["rightkit-qa"] = f.receipts.macos.runtime.crates[CRATE]; delete f.receipts.macos.runtime.crates[CRATE]; }, /no local runtime proof/],
    ["receipt of the wrong OS", (f) => { f.receipts.macos.hostOs = "win32"; }, /no local runtime proof/],
    ["too few repeats", (f) => { for (const w of f.entryOf("windows").workloads) { w.repeats.pop(); w.selected = 1; w.rawSha256 = w.repeats[1].rawSha256; w.record = clone(w.repeats[1].record); } }, /repeat count/],
    ["repeat counts differ between workloads", (f) => { const w = f.entryOf("macos").workloads[1]; w.repeats.push({ ...clone(w.repeats[0]), index: 3 }); }, /repeat count/],
    ["the submitted record is not the worst repeat", (f) => { const w = f.entryOf("windows").workloads[0]; w.selected = 0; w.rawSha256 = w.repeats[0].rawSha256; w.record = clone(w.repeats[0].record); }, /not the worst repeat/],
    ["the submitted record differs from its repeat", (f) => { f.entryOf("windows").workloads[0].record.observed.residentMemoryBytes = 1; }, /refused/],
    ["raw hash does not match the raw text", (f) => { f.entryOf("windows").workloads[0].repeats[0].raw += "x"; }, /raw output or exit status/],
    ["a repeat that failed", (f) => { f.entryOf("windows").workloads[0].repeats[1].exitCode = 1; }, /raw output or exit status/],
    ["repeats whose windows overlap", (f) => { const runs = f.entryOf("macos").workloads[0].repeats; const w = quietWindow(runs[0].window.startedAtMs + 1000); runs[1].window = w; runs[1].startedAt = new Date(w.startedAtMs).toISOString(); runs[1].endedAt = new Date(w.endedAtMs).toISOString(); }, /overlaps the previous repeat/],
  ]) { const f = runtimeFixture(t); mutate(f); await assert.rejects(f.verify(), expected, name); }
});

test("a window that was not quiet is refused, whatever the receipt claims about it", async (t) => {
  const win = (f, key = "windows") => f.entryOf(key).workloads[0].repeats[1].window;
  for (const [name, mutate, expected] of [
    ["broker busy before the run", (f) => { win(f).before.health.inFlight = 1; }, /before: broker inFlight is 1/],
    ["broker queue before the run", (f) => { win(f).before.health.queued = 2; }, /before: broker queued is 2/],
    ["broker paused", (f) => { win(f).before.health.admissionPaused = true; }, /paused or unreported/],
    ["broker not ready", (f) => { win(f).before.health.state = "UNAVAILABLE"; }, /not READY/],
    ["stale broker snapshot", (f) => { win(f).before.health.writtenAtMs -= 60_000; }, /ms old/],
    ["host CPU busy before", (f) => { win(f).before.cpu.idlePercent = 60; }, /before: host CPU was 60% idle/],
    ["host CPU sampled too briefly before", (f) => { win(f).before.cpu.durationMs = 2000; }, /sampled for 2000 ms/],
    ["another job in flight during the run", (f) => { win(f).during.health[2].inFlight = 2; }, /only the measured job may run/],
    ["a job queued during the run", (f) => { win(f).during.health[1].queued = 1; }, /only the measured job may run/],
    ["no snapshots during the run", (f) => { win(f).during.health = []; win(f).before.health.writtenAtMs = win(f).startedAtMs - 3000; win(f).after.health.writtenAtMs = win(f).startedAtMs + 60_000; }, /no broker snapshot for/],
    ["a gap in the snapshots", (f) => { win(f).during.health.splice(1, 4); }, /no broker snapshot for/],
    ["broker not idle after", (f) => { win(f).after.health.inFlight = 1; }, /after: broker inFlight is 1/],
    ["after snapshot written before the run ended", (f) => { win(f).after.health.writtenAtMs = win(f).endedAtMs - 500; }, /written before the run ended/],
    ["host CPU busy after", (f) => { win(f).after.cpu.idlePercent = 40; }, /after: host CPU was 40% idle/],
    ["a foreign job overlapped", (f) => { win(f).jobs.foreign = [{ id: "j-9", workspace: "elsewhere", live: false }]; }, /other root job\(s\) overlapped/],
    ["the measured job not found", (f) => { win(f).jobs.ours = 0; }, /expected exactly 1/],
    ["two jobs match the measured checkout", (f) => { win(f).jobs.ours = 2; }, /expected exactly 1/],
    ["the job listing was not checked", (f) => { win(f).jobs = { checked: false, error: "x" }; }, /not checked/],
    ["the job listing does not span the window", (f) => { win(f).jobs.fromMs += 20_000; }, /does not span/],
    ["wrong window policy", (f) => { win(f).policy = "local-runtime-window/0"; }, /window policy/],
    ["no window evidence", (f) => { delete f.entryOf("macos").workloads[0].repeats[0].window; }, /window evidence is missing/],
  ]) {
    const f = runtimeFixture(t); mutate(f);
    // The receipt keeps claiming quiet: only the recomputation from the samples can refuse it.
    if (f.entryOf("windows").workloads[0].repeats[1].window) assert.equal(win(f).quiet, true);
    await assert.rejects(f.verify(), expected, name);
  }
  assert.deepEqual(windowProblems(quietWindow(T0)), []);
});

test("the trusted minimum-window table names exactly the trusted workloads", () => {
  assert.deepEqual(Object.keys(WORKLOAD_MIN_WINDOW_MS).sort(), Object.entries(RUNTIME_PROOF_WORKLOADS).flatMap(([crate, ids]) => ids.map((id) => `${crate}/${id}`)).sort());
  for (const [crate, ids] of Object.entries(RUNTIME_PROOF_WORKLOADS)) for (const id of ids) assert.ok(minWindowMsFor(crate, id) >= WINDOW_POLICY.minWindowMs, `${crate}/${id}`);
  assert.equal(minWindowMsFor("rightkit-logs", "detached"), 60_000);
  assert.equal(minWindowMsFor("rightkit-unknown", "x"), WINDOW_POLICY.minWindowMs, "an unlisted workload still has the floor");
});

// The reproduction of the Astra review (fa86eeb2, local-runtime-proof.mjs:72): every repeat replaced by a 1 ms window with no
// during-snapshots and a self-reported job count reached "qualified" through verifyLocalRuntimeProof.
test("fabricated 1 ms windows with empty during-snapshots are refused by construction", async (t) => {
  const f = runtimeFixture(t);
  for (const key of ["windows", "macos"]) for (const row of f.entryOf(key).workloads) for (const run of row.repeats) {
    const start = run.window.startedAtMs;
    run.window = { ...quietWindow(start), endedAtMs: start + 1, during: { health: [] } };
    run.endedAt = new Date(start + 1).toISOString();
  }
  await assert.rejects(f.verify(), (error) => /lasted 1 ms, not at least/.test(error.message) && /0 distinct broker snapshot/.test(error.message));
  // Even with a long claimed run the window cannot be shorter than the run it describes.
  const g = runtimeFixture(t);
  const run = g.entryOf("windows").workloads[0].repeats[1];
  run.endedAt = new Date(run.window.endedAtMs + 30_000).toISOString();
  await assert.rejects(g.verify(), /not the span the repeat recorded/);
});

test("a window must last its workload's trusted minimum, hold dense monotonic in-window snapshots, and record the job listing at grant and release", () => {
  const w = (change) => { const x = quietWindow(T0); change(x); return x; };
  const refuses = (window, pattern, context) => { const problems = windowProblems(window, WINDOW_POLICY, context); assert.ok(problems.some((p) => pattern.test(p)), `${pattern}: ${JSON.stringify(problems)}`); };
  // Duration: the floor, the workload minimum and the repeat's own span.
  refuses(quietWindow(T0, 5_000), /lasted 5000 ms, not at least 10000 ms/);
  refuses(quietWindow(T0), /lasted 40000 ms, not at least 60000 ms/, { minWindowMs: minWindowMsFor("rightkit-logs", "detached") });
  assert.deepEqual(windowProblems(quietWindow(T0, 65_000), WINDOW_POLICY, { minWindowMs: 60_000, runStartedAtMs: T0, runEndedAtMs: T0 + 65_000 }), []);
  refuses(quietWindow(T0), /not the span the repeat recorded/, { runStartedAtMs: T0, runEndedAtMs: T0 + 90_000 });
  refuses(quietWindow(T0), /not the span the repeat recorded/, { runStartedAtMs: Number.NaN, runEndedAtMs: T0 + 40_000 });
  // Density, order and placement of the during-snapshots.
  refuses(w((x) => { x.during.health = x.during.health.slice(0, 3); }), /3 distinct broker snapshot\(s\) in a 40000 ms window; at least 7/);
  refuses(w((x) => { x.during.health = []; }), /0 distinct broker snapshot/);
  refuses(w((x) => { x.during.health = x.during.health.map((sample) => ({ ...x.during.health[0] })); }), /not strictly increasing/);
  refuses(w((x) => { x.during.health.reverse(); }), /not strictly increasing/);
  refuses(w((x) => { x.during.health[3] = { ...x.during.health[3], writtenAtMs: x.startedAtMs - 30_000, sampledAtMs: x.startedAtMs - 29_500 }; }), /written outside the run/);
  refuses(w((x) => { x.during.health[3] = { ...x.during.health[3], writtenAtMs: x.endedAtMs + 30_000, sampledAtMs: x.endedAtMs + 30_500 }; }), /written outside the run/);
  // The job listing at grant and at release.
  refuses(w((x) => { delete x.jobs.atGrant; }), /listing at grant was not recorded/);
  refuses(w((x) => { delete x.jobs.atRelease; }), /listing at release was not recorded/);
  refuses(w((x) => { x.jobs.atGrant.checked = false; }), /listing at grant was not recorded/);
  refuses(w((x) => { x.jobs.atGrant.live = ["j-7"]; }), /1 root job\(s\) were live at grant/);
  refuses(w((x) => { x.jobs.atRelease.live = ["j-7"]; }), /1 root job\(s\) were live at release/);
  refuses(w((x) => { x.jobs.atGrant.checkedAtMs = x.startedAtMs - 300_000; }), /at grant was not taken between/);
  refuses(w((x) => { x.jobs.atGrant.checkedAtMs = x.startedAtMs + 5000; }), /at grant was not taken between/);
  refuses(w((x) => { x.jobs.atRelease.checkedAtMs = x.endedAtMs - 5000; }), /at release was not taken after/);
  // The CPU samples lie immediately around the window.
  refuses(w((x) => { x.before.cpu.startedAtMs -= 600_000; x.jobs.fromMs -= 600_000; }), /CPU sample is not immediately before/);
  refuses(w((x) => { x.after.cpu.startedAtMs += 600_000; x.jobs.toMs += 600_000; x.jobs.checkedAtMs += 600_000; x.jobs.atRelease.checkedAtMs += 600_000; }), /CPU sample is not immediately after/);
  assert.deepEqual(windowProblems(quietWindow(T0)), []);
});

test("worst repeat selection follows the budget basis", () => {
  const budget = { ceilings: { residentMemoryBytes: 100, wakeupsPerSecond: 10 }, ceilingBasis: { wakeupsPerSecond: "delta-over-bare" }, deltaCeilings: { wakeupsPerSecond: 2 } };
  const rec = (resident, delta) => ({ observed: { residentMemoryBytes: resident, wakeupsPerSecond: 50 }, delta: { wakeupsPerSecond: delta } });
  assert.equal(worstIndex([rec(10, 0.1), rec(20, 0.1), rec(15, 0.1)], budget), 1);
  assert.equal(worstIndex([rec(10, 1.9), rec(20, 0.1), rec(15, 0.1)], budget), 0, "delta basis: the raw 50 wake-ups never counts, the delta does");
  assert.equal(worstIndex([rec(10, 0.1), rec(10, 0.1)], budget), 0, "ties keep the first");
});

test("local admission qualifies a runtime owner from its pinned receipts, and holds it without them or with a disturbed window", async (t) => {
  const f = runtimeFixture(t), result = await f.admit();
  assert.deepEqual(result.failures, []); assert.equal(result.ok, true, JSON.stringify(result.units));
  const proof = result.units[0].runtimeFeatureProof;
  assert.deepEqual([proof.status, proof.classification, proof.listedOwner, proof.required], ["qualified", "always-on", true, true]);
  assert.deepEqual(proof.proof, { candidateId: ID, proofSha256: localRuntimeProofSha256(f.receipts, CRATE), workloads: ["tracing", "detached"] });
  for (const [name, mutate, expected] of [
    ["no windows runtime receipt", (g) => { delete g.receipts.windows.runtime; }, /local runtime proof: .*no local runtime proof/],
    ["disturbed window", (g) => { g.entryOf("macos").workloads[1].repeats[2].window.after.cpu.idlePercent = 10; }, /local runtime proof: .*not quiet/],
    ["host receipts name different manifests", (g) => { g.receipts.macos.manifestSha256 = "9".repeat(64); }, /different manifests|manifest/],
    ["over budget", (g) => budgetOn(g, (b) => { b.ceilings.residentMemoryBytes = 5; }), /local runtime proof/],
  ]) {
    const g = runtimeFixture(t); mutate(g); const r = await g.admit();
    assert.equal(r.ok, false, name); assert.match(r.units[0].failures.join("\n"), expected, name);
    assert.notEqual(r.units[0].runtimeFeatureProof.status, "qualified", name);
  }
});

test("the credentialed shape check requires the runtime proof on both native hosts and refuses stray or tampered claims", (t) => {
  const f = runtimeFixture(t);
  const check = (mutate = () => {}) => {
    const g = { ...f, receipts: clone(f.receipts) }; mutate(g.receipts);
    const dir = fs.mkdtempSync(path.join(f.temp, "rcpt-"));
    for (const [host, doc] of Object.entries(g.receipts)) if (doc) fs.writeFileSync(path.join(dir, `${host}.json`), JSON.stringify(doc));
    return checkLocalReceipts({ manifest: f.manifest, manifestSha256: f.manifestSha256, receiptsDir: dir, allow: {}, candidate: { id: ID }, linuxRequired: [] });
  };
  assert.deepEqual(check(), []);
  const refused = {
    "no runtime block": (r) => { delete r.windows.runtime; }, "runtime for another unit": (r) => { r.macos.runtime.crates["rightkit-x"] = r.macos.runtime.crates[CRATE]; delete r.macos.runtime.crates[CRATE]; },
    "workload dropped": (r) => { r.macos.runtime.crates[CRATE].workloads.pop(); }, "workload order": (r) => { r.macos.runtime.crates[CRATE].workloads.reverse(); },
    "too few repeats": (r) => { r.windows.runtime.crates[CRATE].workloads[0].repeats.pop(); },
    "not quiet": (r) => { r.windows.runtime.crates[CRATE].workloads[0].repeats[0].window.quiet = false; },
    "raw tampered": (r) => { r.macos.runtime.crates[CRATE].workloads[1].repeats[0].raw += "x"; },
    "selected differs": (r) => { r.macos.runtime.crates[CRATE].workloads[0].selected = 0; },
    "another archive": (r) => { r.windows.runtime.crates[CRATE].archiveSha256 = "1".repeat(64); },
    "another broker": (r) => { r.windows.runtime.crates[CRATE].broker.client = "/evil"; },
    "excluded host": (r) => { r.windows.units[0] = { name: CRATE, version: VERSION, file: f.unit.file, sha256: null, status: "allowed-exclusion", executed: 0, ignored: 0, skipped: 0 }; r.windows.allowHostExclusion = [CRATE]; r.macos.allowHostExclusion = [CRATE]; },
  };
  for (const [name, mutate] of Object.entries(refused)) assert.ok(check(mutate).length > 0, name);
  assert.match(check((r) => { delete r.windows.runtime; }).join("\n"), /carries none for this unit/);
  assert.deepEqual(localRuntimeShapeProblems({ doc: f.receipts.macos, unit: f.unit, hostOs: "darwin", commit: SOURCE, manifestSha256: f.manifestSha256 }), []);
});
