// Local runtime-budget proof of ONE runtime owner on THIS host (Adrian 2026-10-22: runtime proofs no longer need GitHub CI).
// Called by local-qualify.mjs after the host's units are qualified, so the machine is not busy with them. For every trusted
// workload of the crate it runs the pinned Cargo command through the RustRight broker, repeatedly, each time in a quiet window
// whose evidence it records, and returns the entry that goes into this host's `local-verify` receipt as `runtime.crates[crate]`.
//
// Trust and measurement, mirroring the Actions path (scripts/ci/runtime-qualify.mjs):
//   - WHICH workloads run comes from the trusted table in runtime-qualify.mjs (`workloadsFor`), never from the candidate. The
//     candidate's crates/<crate>/benches/workloads.json must equal that table exactly (`readSourcePrescription`) or the run
//     refuses. The harness code is loaded from `root`, a clean checkout whose HEAD is on origin/main (local-qualify.mjs
//     checkoutState) and equals the candidate commit; admission later requires every helper of the harness to equal the
//     promotion checkout's (`harness.helpers` below), exactly as CI requires the producer's blobs to.
//   - WHAT is measured is the candidate's source at `commit`: the checkout is verified clean and at `commit` before and
//     after every run, so the bench cannot have built anything else. Every file the measurement depends on is hashed from
//     the git blobs of `commit` into `bindings`, as CI does (bindSource).
//   - HOW it is measured: the same `RESULT budget` line the bench prints, parsed and recorded by the owner's own
//     `recordMeasurement`, checked by the owner's own `measurementProblems` against the frozen budget (profile, ceilings,
//     required scale). The submitted record is the WORST of `repeats` runs, every run must pass, and each run holds a window
//     that local-runtime-proof.mjs `windowProblems` calls quiet. A disturbed window refuses; it is never retried silently.
// The broker has no client-held exclusive lease (docs/CI.md), so quiet is DETECTED: health snapshots before, during and after,
// the broker's job listing for overlapping root jobs, and host CPU idle before and after.
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { spawnSync } from "./background-process.mjs";
import { spawnCollect } from "./managed-cargo.mjs";
import { discoverBrokerSocket, managedControlCandidates } from "./managed-roots.mjs";

const sha256 = (bytes) => crypto.createHash("sha256").update(bytes).digest("hex");
const sleepFor = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const round3 = (value) => Math.round(value * 1000) / 1000;
const tail = (text, lines = 30) => String(text ?? "").split(/\r?\n/).slice(-lines).join("\n");
const loadFromRoot = (root, ...segments) => import(pathToFileURL(path.join(root, ...segments)).href);
const CREDENTIAL_LIKE = /TOKEN|PASSWORD|SECRET|CREDENTIAL|^ACTIONS_ID_TOKEN/i;
const BROKER_ENV = ["RIGHTKIT_BUILD_BROKER_SOCKET", "RIGHTKIT_CONTROL_ROOT", "RIGHTKIT_DATA_ROOT", "RIGHTKIT_MANAGED_ROOT"];
const BENCH_TIMEOUT_MS = 40 * 60 * 1000;

// ---- host evidence -------------------------------------------------------------------------------

/** Candidate locations of the broker's health.json on this host, most specific first. */
export function brokerHealthFiles({ env = process.env, platform = process.platform, home = os.homedir() } = {}) {
  const join = platform === "win32" ? path.win32.join : path.posix.join;
  const files = [env.RK_BROKER_HEALTH_FILE, ...managedControlCandidates({ env, platform }).map((root) => join(root, "health.json"))];
  if (platform === "darwin") files.push(path.posix.join(home, "Library", "Application Support", "RustRight", "health.json"));
  return [...new Set(files.filter(Boolean))];
}

/** The newest readable broker snapshot, stamped with the time it was read; null when none is readable. */
export function readBrokerHealth({ files, readFile = fs.readFileSync, now = Date.now } = {}) {
  let best = null;
  for (const file of files) {
    let doc;
    try { doc = JSON.parse(readFile(file, "utf8")); } catch { continue; }
    if (doc?.schemaVersion !== 1 || !Number.isInteger(doc.writtenAtMs)) continue;
    if (!best || doc.writtenAtMs > best.writtenAtMs) best = { writtenAtMs: doc.writtenAtMs, sampledAtMs: now(), state: doc.state, admissionPaused: doc.admissionPaused, inFlight: doc.inFlight, queued: doc.queued, admitting: doc.admitting, pid: doc.pid ?? null };
  }
  return best;
}

const cpuTotals = (cpus) => cpus.reduce((sum, cpu) => { const t = cpu.times; sum.idle += t.idle; sum.total += t.user + t.nice + t.sys + t.idle + t.irq; return sum; }, { idle: 0, total: 0 });
/** Host-wide CPU idle share over `ms`, from the OS tick counters (every process, not only ours). */
export async function sampleCpu(ms, { cpus = os.cpus, loadavg = os.loadavg, sleep = sleepFor, now = Date.now, platform = process.platform } = {}) {
  const startedAtMs = now();
  const first = cpuTotals(cpus());
  await sleep(ms);
  const second = cpuTotals(cpus());
  const total = second.total - first.total;
  return { startedAtMs, durationMs: now() - startedAtMs, idlePercent: total > 0 ? round3((100 * (second.idle - first.idle)) / total) : null, cores: cpus().length, loadavg1: platform === "win32" ? null : round3(loadavg()[0]) };
}

/** Root jobs of the broker's listing (`rustright list`): live ones from `inFlight`, finished ones from `recent`. Tolerant of wrapping. */
export function parseBrokerJobs(text) {
  const jobs = []; let found = false;
  const take = (rows, live) => {
    if (!Array.isArray(rows)) return;
    found = true;
    for (const row of rows) {
      const submittedAtMs = Date.parse(row?.submittedAt);
      if (!row || typeof row !== "object" || !Number.isFinite(submittedAtMs)) continue;
      const startedAtMs = row.startedAt ? Date.parse(row.startedAt) : null;
      jobs.push({ id: String(row.id), workspace: row.workspace ?? null, live, submittedAtMs, startedAtMs: Number.isFinite(startedAtMs) ? startedAtMs : null, endedAtMs: live ? null : (Number.isFinite(row.elapsedMs) ? submittedAtMs + row.elapsedMs : null) });
    }
  };
  for (const line of String(text ?? "").split(/\r?\n/)) {
    let doc; try { doc = JSON.parse(line); } catch { continue; }
    for (const holder of [doc, doc?.event, doc?.result]) if (holder && typeof holder === "object") { take(holder.inFlight, true); take(holder.recent, false); }
  }
  if (!found) throw new Error("the broker job listing has no inFlight or recent list");
  return jobs;
}

const samePath = (a, b) => String(a ?? "").replaceAll("\\", "/").replace(/\/+$/, "").toLowerCase() === String(b ?? "").replaceAll("\\", "/").replace(/\/+$/, "").toLowerCase();
/**
 * Which root jobs touched the span [fromMs, toMs]. `ours` counts the root jobs in the measured checkout that were submitted
 * at the run start; anything else that overlaps the span, in any workspace, is foreign. A finished job whose end is unknown
 * is treated as still running (it overlaps).
 */
export function analyzeJobs({ jobs, root, fromMs, toMs, runStartMs, checkedAtMs, slackMs = 1000 }) {
  const mine = jobs.filter((job) => samePath(job.workspace, root) && job.submittedAtMs >= runStartMs - 5000 && job.submittedAtMs <= runStartMs + 60_000);
  const overlaps = (job) => (job.startedAtMs ?? job.submittedAtMs) <= toMs + slackMs && (job.live || job.endedAtMs === null || job.endedAtMs >= fromMs - slackMs);
  const foreign = jobs.filter((job) => overlaps(job) && !(mine.length === 1 && job.id === mine[0].id));
  return { checked: true, fromMs, toMs, checkedAtMs, ours: mine.length, foreign: foreign.map(({ id, workspace, live, startedAtMs, endedAtMs }) => ({ id, workspace, live, startedAtMs, endedAtMs })) };
}

/** The broker listing at one moment, reduced to what the window proof needs: when it was read, which root jobs it names, which are live. */
export const listingRecord = (jobs, checkedAtMs) => ({ checked: true, checkedAtMs, ids: jobs.map((job) => job.id), live: jobs.filter((job) => job.live).map((job) => job.id) });

async function listBrokerJobs({ client, env, run }) {
  const result = await run(client, ["list"], { env, timeout: 30_000, windowsHide: true });
  if (result.error || result.status !== 0) throw new Error(`rustright list failed: ${result.error?.message ?? `exit ${result.status}`}`);
  return parseBrokerJobs(result.stdout);
}

// ---- the proof -----------------------------------------------------------------------------------

async function defaultHarness(root) {
  const [owner, proof, budgets, supplement, workloadEnv] = await Promise.all([
    loadFromRoot(root, "scripts", "ci", "runtime-qualify.mjs"), loadFromRoot(root, "scripts", "ci", "local-runtime-proof.mjs"), loadFromRoot(root, "packages", "release", "runtime-budgets.mjs"),
    loadFromRoot(root, "scripts", "ci", "runtime-supplement.mjs"), loadFromRoot(root, "scripts", "ci", "runtime-workload-env.mjs"),
  ]);
  return { owner, proof, budgets, workloadEnv, helperPaths: supplement.trustedRuntimeHelperPaths };
}

/** `args` of a pinned bench command with `--no-run` before the bench's own `--` arguments: a warm build, never a measurement. */
export const warmArgs = (args) => { const at = args.indexOf("--"); return at < 0 ? [...args, "--no-run"] : [...args.slice(0, at), "--no-run", ...args.slice(at)]; };

/**
 * Measures `crate` on this host and returns its runtime entry (see scripts/ci/runtime-supplement.mjs verifyLocalRuntimeProof
 * for what admission checks). Throws with every reason on the first refusal; nothing partial is returned.
 * options: root, commit, crate, version, archiveSha256, candidateId, broker, cargoCommand, repository, scratch,
 *          repeats (policy range), waitIdleMs (how long to wait for a quiet host before refusing).
 */
export async function measureRuntimeCrate(options, deps = {}) {
  const { root, commit, crate, version, archiveSha256, candidateId, broker, cargoCommand, repository, scratch, repeats, waitIdleMs = 0 } = options;
  const log = deps.log ?? (() => {});
  const platform = deps.platform ?? process.platform;
  const arch = deps.arch ?? process.arch;
  const now = deps.now ?? Date.now;
  const sleep = deps.sleep ?? sleepFor;
  const H = await (deps.loadHarness ?? defaultHarness)(root);
  const { owner, proof } = H;
  const policy = proof.WINDOW_POLICY;
  if (!Number.isInteger(repeats) || repeats < policy.minRepeats || repeats > policy.maxRepeats) throw new Error(`repeats must be an integer from ${policy.minRepeats} to ${policy.maxRepeats}`);
  const gitText = deps.git ?? ((args) => { const r = spawnSync("git", args, { cwd: root, encoding: "utf8", windowsHide: true, maxBuffer: 64 * 1024 * 1024 }); return r.error || r.status !== 0 ? null : (r.stdout ?? "").trim(); });
  const blob = deps.gitBlob ?? ((full) => { const r = spawnSync("git", ["cat-file", "blob", `${commit}:${full}`], { cwd: root, encoding: "buffer", windowsHide: true, maxBuffer: 64 * 1024 * 1024 }); if (r.error || r.status !== 0) throw new Error(`candidate blob missing: ${full}`); return Buffer.from(r.stdout); });
  const lsTree = (paths) => { const text = gitText(["ls-tree", "-r", "--name-only", commit, ...paths]); if (text === null) throw new Error("cannot list the candidate tree"); return text.split(/\r?\n/).filter(Boolean); };
  const assertCheckout = (when) => {
    const head = gitText(["rev-parse", "HEAD"]); const dirty = gitText(["status", "--porcelain"]);
    if (head !== commit) throw new Error(`${when}: HEAD is ${String(head).slice(0, 12)}, not the candidate commit ${commit.slice(0, 12)}; the measured source changed`);
    if (dirty !== "") throw new Error(`${when}: the checkout is not clean, so the measured source is not the candidate:\n${dirty}`);
  };
  assertCheckout("start");

  // Trusted coverage first: a crate without it has nothing to run, and the candidate cannot supply its own list.
  const trusted = owner.workloadsFor(crate);
  const { prescription, workloads } = owner.readSourcePrescription(crate, blob);
  if (JSON.stringify(workloads.map((w) => w.id)) !== JSON.stringify(trusted.map((w) => w.id))) throw new Error("candidate workloads differ from the trusted table");
  const crateDir = path.join(root, "crates", crate);
  let budget;
  try { budget = JSON.parse(blob(`crates/${crate}/runtime-budget.json`)); } catch { throw new Error("candidate runtime-budget.json is missing or not JSON"); }

  const bindings = {};
  for (const full of [...lsTree([`crates/${crate}/src`]), `crates/${crate}/Cargo.toml`]) bindings[full] = sha256(blob(full));
  Object.assign(bindings, owner.bindHarnessMetadata(crate, prescription, blob));
  for (const full of lsTree(["Cargo.toml", "Cargo.lock", "rust-toolchain.toml", ".cargo", `crates/${crate}/benches`, "crates/rightkit-child-process/src", "crates/rightkit-child-process/Cargo.toml"])) bindings[full] = sha256(blob(full));
  const { specs, problems: scopeProblems = [] } = (deps.measuredPathspecs ?? H.budgets.measuredPathspecs)(crateDir);
  if (scopeProblems.length) throw new Error(`cannot bind the measured dependency scope: ${scopeProblems.join("; ")}`);
  for (const full of lsTree(specs)) bindings[full] = sha256(blob(full));
  const helpers = Object.fromEntries(H.helperPaths().map((name) => [name, sha256(blob(name))]));

  const baseEnv = Object.fromEntries(Object.entries(deps.env ?? process.env).filter(([name]) => !CREDENTIAL_LIKE.test(name)));
  const brokerEnv = Object.fromEntries(BROKER_ENV.filter((name) => baseEnv[name]).map((name) => [name, baseEnv[name]]));
  const socket = discoverBrokerSocket(deps.env ?? process.env, { platform });
  if (socket) brokerEnv.RIGHTKIT_BUILD_BROKER_SOCKET = socket;
  const run = deps.run ?? ((command, args, settings) => spawnCollect(command, args, { maxBuffer: 64 * 1024 * 1024, ...settings }));
  const invoke = (args, settings) => run(cargoCommand[0], [...cargoCommand.slice(1), ...args], { cwd: root, windowsHide: true, ...settings });
  const toolchainOf = async () => {
    const rustc = await run(cargoCommand[0] === "rightkit" ? "rightkit" : "rustc", cargoCommand[0] === "rightkit" ? ["rustc", "-Vv"] : ["-Vv"], { cwd: root, env: { ...baseEnv, ...brokerEnv }, windowsHide: true, timeout: 60_000 });
    const cargo = await invoke(["-V"], { env: { ...baseEnv, ...brokerEnv }, timeout: 60_000 });
    if (rustc.error || rustc.status !== 0 || cargo.error || cargo.status !== 0) throw new Error("cannot read the toolchain through the broker");
    return { rustc: String(rustc.stdout).trim(), cargo: String(cargo.stdout).trim() };
  };
  const toolchain = await toolchainOf();

  const healthFiles = deps.healthFiles ?? brokerHealthFiles({ env: deps.env ?? process.env, platform });
  const readHealth = deps.readHealth ?? (() => readBrokerHealth({ files: healthFiles, now }));
  const cpuSample = deps.sampleCpu ?? ((ms) => sampleCpu(ms, { sleep, now, platform }));
  const listJobs = deps.listJobs ?? (() => listBrokerJobs({ client: broker.client, env: { ...baseEnv, ...brokerEnv }, run }));

  async function awaitQuiet(label) {
    const deadline = now() + waitIdleMs;
    for (;;) {
      let problems;
      const first = readHealth();
      problems = first ? proof.brokerIdleProblems(first, "before", policy) : ["before: no broker health snapshot is readable"];
      if (!problems.length) {
        const cpu = await cpuSample(policy.before.minCpuSampleMs + 500);
        const confirm = readHealth();
        problems = [...proof.cpuIdleProblems(cpu, "before", policy.before), ...(confirm ? proof.brokerIdleProblems(confirm, "before", policy) : ["before: no broker health snapshot is readable"])];
        if (!problems.length) return { health: confirm, cpu };
      }
      if (now() >= deadline) throw new Error(`${label}: the host is not quiet${waitIdleMs ? ` after waiting ${Math.round(waitIdleMs / 60_000)} min` : ""}; no measurement was taken: ${problems.join("; ")}`);
      await sleep(5000);
    }
  }

  // The owner recorder rewrites runtime-budget.json and writes an evidence alias inside the checkout, as in CI; it is restored
  // in `finally` and the checkout is verified clean again.
  function recordRun({ workload, raw, index, command }) {
    const budgetFile = path.join(crateDir, "runtime-budget.json");
    const before = fs.readFileSync(budgetFile);
    const alias = path.join(crateDir, "runtime-budget.evidence", `${platform}-${arch}-${version}-synthetic.txt`);
    const prior = fs.existsSync(alias) ? fs.readFileSync(alias) : null;
    const resultFile = path.join(scratch, `${workload.id}-run-${index}.txt`);
    fs.mkdirSync(scratch, { recursive: true });
    fs.writeFileSync(resultFile, raw);
    try {
      const record = (deps.recordMeasurement ?? H.budgets.recordMeasurement)({ crateDir, resultFile, scope: "synthetic", command, description: workload.description });
      return owner.preserveWorkloadRecord(structuredClone(record), workload.id, crate);
    } finally {
      fs.writeFileSync(budgetFile, before);
      if (prior !== null) fs.writeFileSync(alias, prior); else if (fs.existsSync(alias)) fs.unlinkSync(alias);
      fs.rmSync(resultFile, { force: true });
    }
  }

  const entryWorkloads = [];
  for (const workload of trusted) {
    const gate = owner.budgetForWorkload(budget, workload);
    const command = `cargo ${workload.args.join(" ")}`;
    // Warm build first: compilation is never part of a quiet window and never overlaps a measurement.
    log(`[local-runtime] ${crate}/${workload.id}: warm build`);
    const warm = await invoke(warmArgs(workload.args), { env: { ...baseEnv, ...brokerEnv }, timeout: BENCH_TIMEOUT_MS });
    if (warm.error || warm.status !== 0) throw new Error(`${workload.id}: the warm build failed (${warm.error?.message ?? `exit ${warm.status}`})\n${tail(`${warm.stdout}\n${warm.stderr}`)}`);
    const runs = [];
    for (let index = 0; index < repeats; index += 1) {
      assertCheckout(`${workload.id}#${index} before`);
      const label = `${workload.id} run ${index + 1}/${repeats}`;
      log(`[local-runtime] ${crate}/${label}: waiting for a quiet host`);
      const quiet = await awaitQuiet(label);
      // The broker's job listing at grant: read once the host is quiet and before the run starts. Nothing may be live.
      let grantJobs;
      try { grantJobs = listingRecord(await listJobs(), now()); } catch (error) { grantJobs = { checked: false, error: error.message }; }
      let provisioned; let cleanup = null; let result; const during = [];
      const startedAtMs = now();
      try {
        provisioned = H.workloadEnv.provisionWorkloadEnv(workload.env);
        const env = { ...H.workloadEnv.workloadEnvironment(baseEnv, workload.env, provisioned.values), ...brokerEnv, CARGO_INCREMENTAL: "1" };
        const running = invoke(workload.args, { env, timeout: BENCH_TIMEOUT_MS });
        let finished = false;
        running.then(() => { finished = true; }, () => { finished = true; });
        while (!finished) {
          // Only a snapshot the broker WROTE during the run is evidence of it, and each distinct write is kept once.
          const sample = readHealth(); if (sample && sample.writtenAtMs >= startedAtMs && (!during.length || sample.writtenAtMs > during[during.length - 1].writtenAtMs)) during.push(sample);
          await Promise.race([running.catch(() => {}), sleep(1000)]);
        }
        result = await running;
      } catch (error) { result = { error, status: null, signal: null, stdout: "", stderr: `workload environment failed: ${error.message}` }; }
      finally { try { provisioned?.dispose(); } catch (error) { cleanup = error.message; } }
      const endedAtMs = now();
      // Broker idle again, from a snapshot written after the run ended (the broker rewrites health every 5 s).
      let afterHealth = null; const settleBy = now() + 20_000;
      for (;;) {
        const sample = readHealth(); if (sample) afterHealth = sample;
        if (afterHealth && afterHealth.writtenAtMs >= endedAtMs && !proof.brokerIdleProblems(afterHealth, "after", policy).length) break;
        if (now() >= settleBy) break;
        await sleep(1000);
      }
      const afterCpu = await cpuSample(policy.after.minCpuSampleMs + 500);
      const checkedAtMs = now();
      let jobs;
      try {
        const listing = await listJobs();
        jobs = { ...analyzeJobs({ jobs: listing, root, fromMs: quiet.cpu.startedAtMs, toMs: afterCpu.startedAtMs + afterCpu.durationMs, runStartMs: startedAtMs, checkedAtMs }), atGrant: grantJobs, atRelease: listingRecord(listing, checkedAtMs) };
      } catch (error) { jobs = { checked: false, error: error.message }; }
      const window = { policy: policy.id, startedAtMs, endedAtMs, before: { health: quiet.health, cpu: quiet.cpu }, during: { health: during }, after: { health: afterHealth, cpu: afterCpu }, jobs };
      const windowIssues = proof.windowProblems(window, policy, { minWindowMs: proof.minWindowMsFor(crate, workload.id, policy), runStartedAtMs: startedAtMs, runEndedAtMs: endedAtMs });
      window.quiet = windowIssues.length === 0;
      assertCheckout(`${label} after`);
      if (!window.quiet) throw new Error(`${label}: the window was not quiet (disturbed by something on this host; rerun when idle): ${windowIssues.join("; ")}`);
      const raw = `${result.stdout ?? ""}\n${result.stderr ?? ""}`;
      if (cleanup) throw new Error(`${label}: ${cleanup}`);
      if (result.error || result.status !== 0) throw new Error(`${label}: the bench failed (${result.error?.message ?? `exit ${result.status}`})\n${tail(raw)}`);
      let record;
      try { record = recordRun({ workload, raw, index, command }); } catch (error) { throw new Error(`${label}: the owner recorder rejected the run: ${error.message}`); }
      assertCheckout(`${label} recorded`);
      const issues = owner.measurementProblems(record, gate, commit, platform);
      if (issues.length) throw new Error(`${label}: over budget or unproven: ${issues.join("; ")}`);
      runs.push({ index, startedAt: new Date(startedAtMs).toISOString(), endedAt: new Date(endedAtMs).toISOString(), exitCode: result.status, signal: result.signal ?? null, rawSha256: sha256(Buffer.from(raw)), raw, record, window });
      log(`[local-runtime] ${crate}/${label}: ok (${JSON.stringify(record.observed)})`);
    }
    const selected = proof.worstIndex(runs.map((run) => run.record), gate);
    const picked = runs[selected];
    entryWorkloads.push({
      schema: 2, source: "local", workload: workload.id, intervalMs: workload.intervalMs ?? null, crate, version, repository, runId: candidateId, runAttempt: "local", sourceSha: commit,
      artifactId: "local", artifactDigest: null, harnessSha: commit, qualificationRunId: candidateId, qualificationRunAttempt: "local", manifestSha256: null, bindings, toolchain,
      host: { os: platform, arch, release: (deps.release ?? os.release)(), runner: "local" }, command, invocation: [...cargoCommand, ...workload.args].join(" "), envNames: [...workload.env],
      rawFile: `${workload.id}-bench.txt`, rawSha256: picked.rawSha256, exitCode: picked.exitCode, signal: null, problems: [], record: picked.record, selected, repeats: runs,
    });
  }
  assertCheckout("end");
  const cpus = (deps.cpus ?? os.cpus)();
  return {
    schema: 1, kind: "local-runtime-crate", source: "local", policy: policy.id, crate, version, sourceSha: commit, candidateId, manifestSha256: null, archiveSha256,
    host: { os: platform, arch, release: (deps.release ?? os.release)(), model: cpus[0]?.model ?? null, cores: cpus.length }, broker: structuredClone(broker),
    harness: { sourceSha: commit, helpers }, workloads: entryWorkloads,
  };
}

/** Fills in what is only known once the packed manifest is sealed (the host measured before the driver sealed it). */
export function bindRuntimeManifest(runtime, manifestSha256) {
  const bound = structuredClone(runtime);
  for (const entry of Object.values(bound.crates)) {
    entry.manifestSha256 = manifestSha256;
    for (const item of entry.workloads) { item.manifestSha256 = manifestSha256; item.artifactDigest = `local:${manifestSha256}`; }
  }
  return bound;
}
