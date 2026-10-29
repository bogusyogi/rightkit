// The host-side runtime proof: trusted workloads only, quiet windows detected from broker and host evidence, worst repeat
// submitted, and the produced entry admitted by the same verifier the promotion runs. Every effect is faked; no cargo, no broker.
import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { analyzeJobs, bindRuntimeManifest, brokerHealthFiles, measureRuntimeCrate, parseBrokerJobs, readBrokerHealth, sampleCpu, warmArgs } from "./local-runtime.mjs";
import { workloadsFor, supportedPrescription } from "../../scripts/ci/runtime-qualify.mjs";
import { verifyLocalRuntimeProof, authenticatedRuntimeProof } from "../../scripts/ci/runtime-supplement.mjs";
import { localAssetName, localTag } from "../../scripts/ci/reviewed-candidate.mjs";
import { parseBudgetResult } from "./runtime-budgets.mjs";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const CRATE = "rightkit-logs", VERSION = "0.3.4", COMMIT = "a".repeat(40), ID = "local-logs-034-aaaaaa", T0 = 1_800_000_000_000;
const hash = (bytes) => crypto.createHash("sha256").update(bytes).digest("hex");
const temp = (t) => { const dir = fs.realpathSync.native(fs.mkdtempSync(path.join(process.env.RIGHTKIT_TEST_SCRATCH ?? os.tmpdir(), "rk-lr-"))); t.after(() => fs.rmSync(dir, { recursive: true, force: true })); return dir; };

function virtualTree() {
  const budget = JSON.parse(fs.readFileSync(path.join(ROOT, "crates", CRATE, "runtime-budget.json"), "utf8"));
  budget.status = { api: "status()", file: "src/lib.rs", exposes: ["residentMemoryBytes"] }; budget.measurements = [];
  return {
    // Every crate is its own Cargo workspace with its own Cargo.lock (#1285).
    [`crates/${CRATE}/Cargo.lock`]: `# Cargo.lock\nversion = 3\n\n[[package]]\nname = "${CRATE}"\nversion = "${VERSION}"\n`,
    [`crates/${CRATE}/Cargo.toml`]: `[package]\nname = "${CRATE}"\nversion = "${VERSION}"\nedition = "2021"\n\n[workspace]\n`, [`crates/${CRATE}/src/lib.rs`]: "pub fn status() {}\n", [`crates/${CRATE}/src/detached.rs`]: "pub struct DetachedLog;\n",
    [`crates/${CRATE}/README.md`]: "## Runtime budget\n\nfixture\n", [`crates/${CRATE}/runtime-budget.json`]: JSON.stringify(budget), [`crates/${CRATE}/benches/workloads.json`]: JSON.stringify(supportedPrescription(CRATE)),
    [`crates/${CRATE}/benches/budget.rs`]: "// tracing\n", [`crates/${CRATE}/benches/budget_detached.rs`]: "// detached\n",
  };
}

/** A host whose clock, broker, CPU and bench are all scripted. `plan` bends it: busy broker, foreign job, hot bench, dirty tree... */
function host(t, { platform = "win32", arch = "x64", plan = {} } = {}) {
  const files = { ...virtualTree(), ...(plan.files ?? {}) };
  const scratch = temp(t);
  const state = { clock: T0, running: false, benches: 0, calls: [], benchStart: null, benchEnd: null, dirty: false, jobs: [], idleCalls: 0 };
  const git = (args) => {
    if (args[0] === "rev-parse") return plan.head ?? COMMIT;
    if (args[0] === "status") return state.dirty ? " M crates/x" : "";
    if (args[0] === "ls-tree") { const paths = args.slice(4); return Object.keys(files).filter((name) => paths.some((p) => name === p || name.startsWith(`${p}/`))).join("\n"); }
    throw new Error(`unexpected git ${args.join(" ")}`);
  };
  const gitBlob = (full) => (Object.hasOwn(files, full) ? Buffer.from(files[full]) : fs.readFileSync(path.join(ROOT, full)));
  // The broker rewrites health.json every 5 s: a read between two writes returns the earlier one.
  const snapshot = () => ({ writtenAtMs: Math.floor(state.clock / 5000) * 5000, sampledAtMs: state.clock, state: "READY", admissionPaused: plan.paused ?? false, inFlight: state.running ? 1 + (plan.during?.(state) ?? 0) : (plan.busyBefore && state.benches === 0 && state.idleCalls++ < plan.busyBefore ? 2 : (plan.afterInFlight && state.benchEnd ? plan.afterInFlight : 0)), queued: plan.queued ?? 0, admitting: 0, pid: 1 });
  const run = async (command, args) => {
    state.calls.push([command, ...args]);
    if (args.includes("-V") || args.includes("-Vv")) return { status: 0, stdout: "tool fixture\n", stderr: "" };
    if (args.includes("--no-run")) return { status: 0, stdout: "built\n", stderr: "" };
    // The bench: forty 1 s steps (seventy for the detached drainer, whose trusted idle observation is 60 s), each yielding to the sampling loop.
    state.running = true; state.benchStart = state.clock; const n = state.benches++;
    const steps = args.some((arg) => String(arg).includes("budget_detached")) ? 70 : 40;
    for (let step = 0; step < steps; step += 1) { state.clock += 1000; await new Promise((resolve) => setImmediate(resolve)); }
    if (plan.dirtyDuringRun) state.dirty = true;
    state.running = false; state.benchEnd = state.clock;
    const hot = plan.hot?.(n) ?? 100 + 10 * (n % 3);
    return { status: plan.benchStatus ?? 0, signal: null, stdout: `compiling...\nRESULT budget crate=${CRATE} version=${VERSION} resident_bytes=${hot} idle_cpu_percent=0 wakeups_per_s=0 disk_ops_per_s=${platform === "darwin" ? "unavailable" : "0"} scale_count=200000 scale_unit=lines\n`, stderr: "" };
  };
  const deps = {
    platform, arch, git, gitBlob, run, now: () => state.clock, sleep: async (ms) => { state.clock += ms; await new Promise((resolve) => setImmediate(resolve)); }, readHealth: snapshot, release: () => "10.0", cpus: () => [{ model: "fixture cpu" }],
    sampleCpu: async (ms) => { const startedAtMs = state.clock; state.clock += ms; return { startedAtMs, durationMs: ms, idlePercent: plan.cpuIdle?.(state) ?? 98, cores: 8, loadavg1: null }; },
    listJobs: async () => {
      if (plan.listFails) throw new Error("rustright list failed");
      if (plan.liveAtGrant && state.benchStart === null) return [{ id: "stray", workspace: "/elsewhere", live: true, submittedAtMs: state.clock - 1000, startedAtMs: state.clock - 1000, endedAtMs: null }];
      const mine = state.benchStart === null ? [] : [{ id: "mine", workspace: ROOT, live: false, submittedAtMs: state.benchStart, startedAtMs: state.benchStart, endedAtMs: state.benchEnd }];
      return [...mine, ...(plan.foreign ? [{ id: "other", workspace: "/elsewhere", live: false, submittedAtMs: state.benchStart + 1000, startedAtMs: state.benchStart + 1000, endedAtMs: state.benchEnd - 1000 }] : [])];
    },
    recordMeasurement: ({ resultFile, command, description }) => {
      const raw = fs.readFileSync(resultFile); const parsed = parseBudgetResult(raw.toString("utf8")); if (!parsed.ok) throw new Error(parsed.problems.join("; "));
      const v = parsed.value;
      return { host: { os: platform, arch }, crateVersion: VERSION, commit: COMMIT, date: "2026-10-22", scope: "synthetic", dataScale: { description, unit: v.scale_unit, count: v.scaleCount }, command, observed: { residentMemoryBytes: v.residentMemoryBytes, idleCpuPercent: v.idleCpuPercent, wakeupsPerSecond: v.wakeupsPerSecond, diskOpsPerSecond: v.diskOpsPerSecond }, evidence: { file: `runtime-budget.evidence/${platform}-${arch}-${VERSION}-synthetic.txt`, sha256: hash(raw) } };
    },
    measuredPathspecs: () => ({ specs: [`crates/${CRATE}/src`], problems: [] }), healthFiles: ["unused"], env: { PATH: process.env.PATH, GITHUB_TOKEN: "secret", RIGHTKIT_EXCLUSIVE: "x" }, log: () => {},
  };
  const options = { root: ROOT, commit: COMMIT, crate: CRATE, version: VERSION, archiveSha256: "d".repeat(64), candidateId: ID, broker: { name: "rustright", client: `/rustright-${platform}`, sha256: "9".repeat(64) }, cargoCommand: ["rustright", "cargo"], repository: "bogusyogi/rightkit", scratch, repeats: 3, waitIdleMs: 0, ...(plan.options ?? {}) };
  return { state, deps: { ...deps, ...(plan.deps ?? {}) }, options, files };
}

test("one host: trusted workloads only, warm build then three quiet runs each, worst submitted, no scratch left, tree untouched", async (t) => {
  const h = host(t); const entry = await measureRuntimeCrate(h.options, h.deps);
  assert.deepEqual(entry.workloads.map((w) => w.workload), workloadsFor(CRATE).map((w) => w.id));
  for (const w of entry.workloads) {
    assert.equal(w.repeats.length, 3); assert.equal(w.selected, 2); assert.equal(w.record.observed.residentMemoryBytes, 120);
    assert.equal(w.command, `cargo ${workloadsFor(CRATE).find((x) => x.id === w.workload).args.join(" ")}`);
    assert.ok(w.repeats.every((run) => run.window.quiet === true && run.exitCode === 0 && hash(Buffer.from(run.raw)) === run.rawSha256));
    // Duration, density and the job listing at grant and at release are part of the recorded evidence.
    assert.ok(w.repeats.every((run) => run.window.endedAtMs - run.window.startedAtMs >= 40_000 && run.window.during.health.length >= 7 && run.window.during.health.every((sample, i, all) => sample.writtenAtMs >= run.window.startedAtMs && (i === 0 || sample.writtenAtMs > all[i - 1].writtenAtMs))));
    assert.ok(w.repeats.every((run) => run.window.jobs.atGrant.checked && run.window.jobs.atGrant.live.length === 0 && run.window.jobs.atRelease.checked && run.window.jobs.atRelease.live.length === 0 && run.window.jobs.atRelease.checkedAtMs >= run.window.endedAtMs));
  }
  // Commands: toolchain through the broker, then per workload one warm build and three runs, all through `rustright cargo`.
  const benches = h.state.calls.filter((call) => call.includes("--bench") && !call.includes("--no-run"));
  assert.equal(benches.length, 6); assert.ok(h.state.calls.every((call) => call[0] === "rustright"));
  assert.equal(h.state.calls.filter((call) => call.includes("--no-run")).length, 2);
  assert.deepEqual(benches[0].slice(0, 3), ["rustright", "cargo", "test"]); assert.ok(benches[0].includes("--locked") && benches[0].includes("release-iterate"));
  assert.equal(entry.harness.sourceSha, COMMIT); assert.ok(entry.harness.helpers["scripts/ci/runtime-qualify.mjs"]); assert.ok(entry.workloads[0].bindings[`crates/${CRATE}/benches/workloads.json`]);
  assert.equal(entry.manifestSha256, null, "bound only when the driver seals the manifest");
  assert.deepEqual(fs.readdirSync(h.options.scratch), [], "the raw result scratch is removed");
});

test("the entry the host writes is admitted by the promotion's verifier on both hosts, and refused when its window is disturbed", async (t) => {
  const manifestSha256 = "e".repeat(64); const unitSha = "d".repeat(64);
  const make = async (platform, arch, plan) => { const h = host(t, { platform, arch, plan }); return { h, entry: await measureRuntimeCrate(h.options, h.deps) }; };
  const win = await make("win32", "x64"), mac = await make("darwin", "arm64");
  const files = win.h.files;
  const source = { source: { sha: COMMIT, tree: "c".repeat(40) }, root: path.join(temp(t), "source"), readData: (name) => { if (Object.hasOwn(files, name)) return Buffer.from(files[name]); try { return fs.readFileSync(path.join(ROOT, name)); } catch { throw new Error(`candidate missing ${name}`); } } };
  const names = Object.fromEntries(["packed", "windows", "macos"].map((kind) => [kind, { name: localAssetName(ID, kind), sha256: hash(Buffer.from(kind)) }]));
  const candidate = { id: ID, source: "local", sourceSha: COMMIT, registry: "crates", units: [CRATE], allow: { hostExclusion: [], noTest: [], ignored: [], missingAssets: [] }, release: { tag: localTag(ID) }, assets: names };
  const unit = { name: CRATE, version: VERSION, dir: `crates/${CRATE}`, sha256: unitSha };
  const receipt = (key, hostOs, h) => ({ hostOs, arch: h.entry.host.arch, broker: h.entry.broker, runtime: bindRuntimeManifest({ schema: 1, kind: "local-runtime", crates: { [CRATE]: h.entry } }, manifestSha256) });
  const receipts = { windows: receipt("windows", "win32", win), macos: receipt("macos", "darwin", mac) };
  const handle = await verifyLocalRuntimeProof({ candidate, manifest: { commit: COMMIT }, manifestSha256, unit, source, receipts });
  assert.equal(authenticatedRuntimeProof(handle, candidate, unit).length, 4);
  // The unbound entry (no manifest hash yet) is not a proof.
  await assert.rejects(verifyLocalRuntimeProof({ candidate, manifest: { commit: COMMIT }, manifestSha256, unit, source, receipts: { windows: { ...receipts.windows, runtime: { schema: 1, kind: "local-runtime", crates: { [CRATE]: win.entry } } }, macos: receipts.macos } }), /refused/);
  // A different source blob than the host measured is refused.
  const drift = { ...source, readData: (name) => (name === `crates/${CRATE}/src/detached.rs` ? Buffer.from("pub struct Other;\n") : source.readData(name)) };
  await assert.rejects(verifyLocalRuntimeProof({ candidate, manifest: { commit: COMMIT }, manifestSha256, unit, source: drift, receipts }), /source binding differs/);
});

test("a host that is not quiet refuses before measuring, and says why", async (t) => {
  const busy = host(t, { plan: { busyBefore: 3 } });
  await assert.rejects(measureRuntimeCrate(busy.options, busy.deps), /the host is not quiet; no measurement was taken: before: broker inFlight is 2/);
  assert.equal(busy.state.benches, 0);
  const paused = host(t, { plan: { paused: true } });
  await assert.rejects(measureRuntimeCrate(paused.options, paused.deps), /paused or unreported/);
  const hotCpu = host(t, { plan: { cpuIdle: () => 55 } });
  await assert.rejects(measureRuntimeCrate(hotCpu.options, hotCpu.deps), /before: host CPU was 55% idle/);
  // Waiting is bounded: with a wait budget the same busy broker that clears is accepted.
  const clears = host(t, { plan: { busyBefore: 3, options: { waitIdleMs: 120_000 } } });
  const entry = await measureRuntimeCrate(clears.options, clears.deps);
  assert.equal(entry.workloads.length, 2);
});

test("a window disturbed during or after the run refuses; so does an unreadable job listing", async (t) => {
  for (const [name, plan, pattern] of [
    ["another job in flight during the run", { during: () => 1 }, /only the measured job may run/],
    ["a job queued during the run", { queued: 1 }, /queued is 1|only the measured job may run/],
    ["broker not idle afterwards", { afterInFlight: 1 }, /after: broker inFlight is 1/],
    ["a foreign root job overlapped the run", { foreign: true }, /other root job\(s\) overlapped/],
    ["the job listing is unavailable", { listFails: true }, /job listing was not checked/],
    ["a root job was live at grant", { liveAtGrant: true }, /root job\(s\) were live at grant/],
  ]) {
    const h = host(t, { plan }); await assert.rejects(measureRuntimeCrate(h.options, h.deps), pattern, name);
  }
});

test("a dirty or moved checkout, a failed bench, an over-budget run or a changed prescription refuses", async (t) => {
  const moved = host(t, { plan: { head: "b".repeat(40) } }); await assert.rejects(measureRuntimeCrate(moved.options, moved.deps), /HEAD is bbbbbbbbbbbb, not the candidate commit/);
  const dirty = host(t, { plan: { dirtyDuringRun: true } }); await assert.rejects(measureRuntimeCrate(dirty.options, dirty.deps), /not clean, so the measured source is not the candidate/);
  const failing = host(t, { plan: { benchStatus: 101 } }); await assert.rejects(measureRuntimeCrate(failing.options, failing.deps), /the bench failed \(exit 101\)/);
  const hot = host(t, { plan: { hot: (n) => (n === 1 ? 900_000_000 : 100) } }); await assert.rejects(measureRuntimeCrate(hot.options, hot.deps), /over budget or unproven: .*residentMemoryBytes/);
  const prescribed = JSON.parse(virtualTree()[`crates/${CRATE}/benches/workloads.json`]); prescribed.workloads[0].args.push("1");
  const altered = host(t, { plan: { files: { [`crates/${CRATE}/benches/workloads.json`]: JSON.stringify(prescribed) } } });
  await assert.rejects(measureRuntimeCrate(altered.options, altered.deps), /differs from trusted supported coverage/);
  const uncovered = host(t, { plan: { options: { crate: "rightkit-fsindex" } } }); await assert.rejects(measureRuntimeCrate(uncovered.options, uncovered.deps), /no trusted supported coverage/);
  const fewer = host(t, { plan: { options: { repeats: 2 } } }); await assert.rejects(measureRuntimeCrate(fewer.options, fewer.deps), /repeats must be an integer from 3 to 9/);
});

test("helpers: health file choice, CPU idle, job listing parse and overlap analysis, warm build arguments", async () => {
  assert.ok(brokerHealthFiles({ env: {}, platform: "win32" }).some((file) => /health\.json$/.test(file)));
  assert.ok(brokerHealthFiles({ env: { RK_BROKER_HEALTH_FILE: "/x/h.json" }, platform: "darwin", home: "/Users/a" })[0] === "/x/h.json");
  const docs = { a: { schemaVersion: 1, writtenAtMs: 5, state: "READY", admissionPaused: false, inFlight: 0, queued: 0, admitting: 0 }, b: { schemaVersion: 1, writtenAtMs: 9, state: "READY", admissionPaused: false, inFlight: 1, queued: 0, admitting: 0 }, c: { schemaVersion: 2, writtenAtMs: 99 } };
  const read = (file) => { if (!docs[file]) throw new Error("missing"); return JSON.stringify(docs[file]); };
  assert.equal(readBrokerHealth({ files: ["a", "b", "c", "none"], readFile: read, now: () => 1000 }).inFlight, 1);
  assert.equal(readBrokerHealth({ files: ["c", "none"], readFile: read }), null);
  let tick = 0; const cpus = () => [{ times: { user: 10 + tick * 5, nice: 0, sys: 5 + tick * 5, idle: 85 + tick * 90, irq: 0 } }];
  const sample = await sampleCpu(10, { cpus: () => { const value = cpus(); tick += 1; return value; }, sleep: async () => {}, platform: "win32" });
  assert.equal(sample.idlePercent, 90);
  const listing = JSON.stringify({ type: "LIST", inFlight: [{ id: "j1", workspace: "D:\\Claude\\rightkit", submittedAt: "2026-10-22T10:00:00Z", startedAt: "2026-10-22T10:00:01Z" }], recent: [{ id: "j0", workspace: "/w", submittedAt: "2026-10-22T09:00:00Z", startedAt: "2026-10-22T09:00:01Z", elapsedMs: 5000 }] });
  const jobs = parseBrokerJobs(`noise\n${listing}\n`);
  assert.deepEqual(jobs.map((j) => [j.id, j.live]), [["j1", true], ["j0", false]]);
  assert.throws(() => parseBrokerJobs("nothing"), /no inFlight or recent list/);
  const at = Date.parse("2026-10-22T10:00:00Z");
  const verdict = analyzeJobs({ jobs, root: "d:/claude/rightkit", fromMs: at - 10_000, toMs: at + 50_000, runStartMs: at, checkedAtMs: at + 60_000 });
  assert.deepEqual([verdict.ours, verdict.foreign.length], [1, 0]);
  assert.equal(analyzeJobs({ jobs, root: "/elsewhere", fromMs: at - 10_000, toMs: at + 50_000, runStartMs: at, checkedAtMs: at }).foreign.length, 1, "a live job in another workspace overlaps");
  assert.deepEqual(warmArgs(["test", "-p", "x", "--bench", "b", "--", "10", "30"]), ["test", "-p", "x", "--bench", "b", "--no-run", "--", "10", "30"]);
  assert.deepEqual(warmArgs(["test", "--bench", "b"]), ["test", "--bench", "b", "--no-run"]);
});
