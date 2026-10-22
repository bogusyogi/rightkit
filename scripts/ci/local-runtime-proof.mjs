// Policy and verifier-side checks for a LOCAL runtime-budget proof (source "local"). Pure functions over recorded DATA:
// no I/O, no candidate code, no imports. The producer (packages/release/local-runtime.mjs) loads this file from the
// checkout it measures; admission (runtime-supplement.mjs verifyLocalRuntimeProof) imports it from the trusted
// promotion checkout and RECOMPUTES every verdict from the recorded samples. A receipt's own `quiet` flag is never trusted.
//
// What a quiet window is. The budget figures of a runtime workload are the bench process's own resident memory, CPU
// share, wake-ups and disk operations. Foreign load on the host cannot add to them directly, but it can steal the CPU the
// bench needs to settle, delay its timers and (on a shared build host) start compilers that touch the same disk. So a
// proof is admitted only when, from the moment the host went quiet until after the bench ended:
//   - the broker reported itself READY, not paused, with no root job in flight, queued or admitting before the run;
//   - during the run every broker snapshot shows at most the measured job itself (inFlight <= 1, nothing queued);
//   - the broker reported idle again after the run, from a snapshot written after the run ended;
//   - the host CPU was at least `minCpuIdlePercent` idle for a sampled stretch before and after the run;
//   - the broker's own job listing shows exactly one root job in the measured checkout inside the span and no other root
//     job overlapping it (this catches a short job that two 5 s health snapshots could miss).
// The broker offers no client-held exclusive lease (see docs/CI.md "Local qualification"), so this is detection, not
// prevention: a window that was disturbed is refused, never repaired.

// A window is also a DURATION and a DENSITY, never only a verdict (policy /2):
//   - it lasts at least `minWindowMs` (a 10 s floor) and at least the trusted idle observation of its workload
//     (WORKLOAD_MIN_WINDOW_MS below, taken from the trusted workload descriptions), and at least as long as the repeat's own
//     recorded start/end, which the window must equal; a fabricated millisecond window is refused by construction;
//   - the broker snapshots recorded during the run are distinct, strictly increasing in `writtenAtMs`, all written inside the
//     window, and at least one per `snapshotEveryMs` of window (the broker rewrites health.json every 5 s);
//   - the broker job listing is recorded at grant (just before the run starts) and at release (just after it ended), and no
//     root job is live at either moment;
//   - the host CPU samples lie immediately before and immediately after the window, not at arbitrary times.
export const WINDOW_POLICY = Object.freeze({
  id: "local-runtime-window/2",
  // Shortest window of any workload: even a workload whose own idle phase is short observes for this long.
  minWindowMs: 10_000,
  // Every workload is measured this many times, each in its own quiet window; the WORST run is the submitted record and
  // every run must pass the full budget gate. One lucky quiet run therefore cannot hide a noisy workload.
  minRepeats: 3,
  maxRepeats: 9,
  // The broker rewrites health.json every 5 s; a snapshot older than this is not evidence of the present.
  maxHealthAgeMs: 20_000,
  before: Object.freeze({ minCpuSampleMs: 10_000, minCpuIdlePercent: 90, maxStartLagMs: 120_000 }),
  during: Object.freeze({ maxInFlight: 1, maxAdmitting: 1, maxSnapshotGapMs: 15_000, snapshotEveryMs: 5_000 }),
  after: Object.freeze({ minCpuSampleMs: 5_000, minCpuIdlePercent: 90, maxStartLagMs: 60_000 }),
  // Slack between two clocks read in one process (sample start versus run start).
  skewMs: 1_000,
});

// The shortest window each trusted workload may be measured in: the idle observation its trusted description (the table in
// runtime-qualify.mjs) states ("then 30 s idle", "idle >=60 s", "5 s settling, then 30 s observation"). A workload whose
// description names no idle length ("default idle window", "then idle") is bound by the policy floor alone. A test asserts
// this table names exactly the workloads of RUNTIME_PROOF_WORKLOADS, so a new workload cannot be added without a minimum.
export const WORKLOAD_MIN_WINDOW_MS = Object.freeze({
  "rightkit-logs/tracing": 10_000,
  "rightkit-logs/detached": 60_000,
  "rightkit-job-queue/budget": 10_000,
  "rightkit-job-queue/pool_budget": 30_000,
  "rightkit-qa/budget": 30_000,
  "rightkit-daemon/local": 30_000,
  "rightkit-daemon/remote": 30_000,
  "rightkit-daemon/remote-loopback": 30_000,
  "rightkit-tauri-shell/native-shell-wv2-0_38": 35_000,
  "rightkit-tauri-shell/native-shell-wv2-0_39": 35_000,
  "rightkit-tauri-shell/native-host-wv2-0_38": 35_000,
  "rightkit-tauri-shell/native-host-wv2-0_39": 35_000,
});
/** The least window duration admitted for one workload: the larger of the policy floor and the trusted idle observation. */
export const minWindowMsFor = (crate, workload, policy = WINDOW_POLICY) => Math.max(policy.minWindowMs, WORKLOAD_MIN_WINDOW_MS[`${crate}/${workload}`] ?? 0);

const FIGURES = Object.freeze(["residentMemoryBytes", "idleCpuPercent", "wakeupsPerSecond", "diskOpsPerSecond"]);
const isInt = Number.isInteger;
const isNum = (value) => typeof value === "number" && Number.isFinite(value);

function snapshotProblems(health, label, policy) {
  if (!health || typeof health !== "object" || Array.isArray(health)) return [`${label}: no broker health snapshot`];
  const problems = [];
  if (!isInt(health.writtenAtMs) || !isInt(health.sampledAtMs)) return [`${label}: the broker health snapshot has no timestamps`];
  if (health.sampledAtMs - health.writtenAtMs > policy.maxHealthAgeMs) problems.push(`${label}: the broker health snapshot is ${health.sampledAtMs - health.writtenAtMs} ms old`);
  if (health.sampledAtMs - health.writtenAtMs < -policy.skewMs) problems.push(`${label}: the broker health snapshot is dated after it was read`);
  if (health.state !== "READY") problems.push(`${label}: broker state is ${JSON.stringify(health.state)}, not READY`);
  if (health.admissionPaused !== false) problems.push(`${label}: broker admission is paused or unreported`);
  for (const key of ["inFlight", "queued", "admitting"]) if (!isInt(health[key]) || health[key] < 0) problems.push(`${label}: broker ${key} is not reported`);
  return problems;
}
const idleProblems = (health, label, policy) => {
  const problems = snapshotProblems(health, label, policy);
  if (problems.length) return problems;
  return ["inFlight", "queued", "admitting"].filter((key) => health[key] !== 0).map((key) => `${label}: broker ${key} is ${health[key]}, not 0`);
};
function cpuProblems(cpu, label, rule) {
  if (!cpu || typeof cpu !== "object") return [`${label}: no host CPU sample`];
  const problems = [];
  if (!isNum(cpu.startedAtMs) || !isNum(cpu.durationMs) || cpu.durationMs < rule.minCpuSampleMs) problems.push(`${label}: host CPU was sampled for ${cpu.durationMs} ms, not at least ${rule.minCpuSampleMs} ms`);
  if (!isNum(cpu.idlePercent) || cpu.idlePercent < rule.minCpuIdlePercent || cpu.idlePercent > 100) problems.push(`${label}: host CPU was ${cpu.idlePercent}% idle, not at least ${rule.minCpuIdlePercent}%`);
  return problems;
}

/** Health problems that make a host not yet quiet. Used by the producer while it waits, and by the verifier. */
export const brokerIdleProblems = (health, label = "broker", policy = WINDOW_POLICY) => idleProblems(health, label, policy);
export const cpuIdleProblems = (cpu, label, rule) => cpuProblems(cpu, label, rule);

/**
 * Every reason the recorded window is not a quiet one. Empty means quiet. Recomputed from the samples, never from `quiet`.
 * `context` carries what only the caller knows about THIS run: `minWindowMs` (the workload's trusted minimum, see
 * minWindowMsFor; the policy floor applies when absent) and `runStartedAtMs`/`runEndedAtMs` (the repeat's own recorded
 * start and end, which the window must equal).
 */
export function windowProblems(window, policy = WINDOW_POLICY, context = {}) {
  if (!window || typeof window !== "object" || Array.isArray(window)) return ["window evidence is missing"];
  const problems = [];
  if (window.policy !== policy.id) problems.push(`window policy ${JSON.stringify(window.policy)} is not ${policy.id}`);
  const { startedAtMs: start, endedAtMs: end, before, during, after, jobs } = window;
  if (!isNum(start) || !isNum(end) || !(end > start)) return [...problems, "window start/end are invalid"];
  const duration = end - start;
  const requiredMs = Math.max(policy.minWindowMs, isNum(context?.minWindowMs) ? context.minWindowMs : 0);
  if (duration < requiredMs) problems.push(`window: lasted ${duration} ms, not at least ${requiredMs} ms`);
  if (context && (context.runStartedAtMs !== undefined || context.runEndedAtMs !== undefined)) {
    if (!isNum(context.runStartedAtMs) || !isNum(context.runEndedAtMs) || Math.abs(context.runStartedAtMs - start) > policy.skewMs || Math.abs(context.runEndedAtMs - end) > policy.skewMs) problems.push("window: it is not the span the repeat recorded for its own run");
    else if (duration < context.runEndedAtMs - context.runStartedAtMs - policy.skewMs) problems.push("window: it is shorter than the run it claims to measure");
  }
  problems.push(...idleProblems(before?.health, "before", policy));
  if (before?.health && isInt(before.health.sampledAtMs) && (before.health.sampledAtMs > start + policy.skewMs || start - before.health.sampledAtMs > policy.before.maxStartLagMs)) problems.push("before: the broker snapshot is not immediately before the run");
  problems.push(...cpuProblems(before?.cpu, "before", policy.before));
  if (isNum(before?.cpu?.startedAtMs) && isNum(before?.cpu?.durationMs)) {
    const cpuEnd = before.cpu.startedAtMs + before.cpu.durationMs;
    if (cpuEnd > start + policy.skewMs) problems.push("before: the host CPU sample overlaps the run");
    else if (start - cpuEnd > policy.before.maxStartLagMs) problems.push("before: the host CPU sample is not immediately before the run");
  }
  problems.push(...idleProblems(after?.health, "after", policy));
  if (after?.health && isInt(after.health.writtenAtMs) && after.health.writtenAtMs < end) problems.push("after: the broker idle snapshot was written before the run ended");
  problems.push(...cpuProblems(after?.cpu, "after", policy.after));
  if (isNum(after?.cpu?.startedAtMs)) {
    if (after.cpu.startedAtMs < end - policy.skewMs) problems.push("after: the host CPU sample overlaps the run");
    else if (after.cpu.startedAtMs - end > policy.after.maxStartLagMs) problems.push("after: the host CPU sample is not immediately after the run");
  }
  const samples = Array.isArray(during?.health) ? during.health : null;
  if (!samples) problems.push("during: no broker snapshots were recorded");
  else {
    // Density: the broker rewrites health.json every snapshotEveryMs, so a real run of this length holds that many distinct
    // snapshots; one is allowed to fall just outside each edge. An empty list is therefore never enough.
    const needed = Math.max(1, Math.floor(duration / policy.during.snapshotEveryMs) - 1);
    const seen = new Map();
    let previous = null;
    for (const [index, sample] of samples.entries()) {
      const label = `during[${index}]`;
      const bad = snapshotProblems(sample, label, policy);
      problems.push(...bad);
      if (bad.length) continue;
      if (sample.inFlight > policy.during.maxInFlight || sample.admitting > policy.during.maxAdmitting || sample.queued !== 0) problems.push(`${label}: broker shows inFlight ${sample.inFlight}, queued ${sample.queued}, admitting ${sample.admitting}; only the measured job may run`);
      if (sample.sampledAtMs < start - policy.skewMs || sample.sampledAtMs > end + policy.skewMs) problems.push(`${label}: snapshot was read outside the run`);
      // Written, not only read, inside the window: a snapshot the broker wrote before the run began says nothing about the run.
      if (sample.writtenAtMs < start - policy.skewMs || sample.writtenAtMs > end + policy.skewMs) problems.push(`${label}: snapshot was written outside the run`);
      if (previous && (sample.writtenAtMs <= previous.writtenAtMs || sample.sampledAtMs < previous.sampledAtMs)) problems.push(`${label}: broker snapshots are not strictly increasing in writtenAtMs`);
      previous = sample;
      seen.set(sample.writtenAtMs, sample);
    }
    if (seen.size < needed) problems.push(`during: ${seen.size} distinct broker snapshot(s) in a ${duration} ms window; at least ${needed} (one per ${policy.during.snapshotEveryMs} ms) are required`);
    // The snapshots must cover the run: before, every distinct one during, after; no stretch longer than the allowed gap.
    const chain = [before?.health, ...seen.values(), after?.health].filter((sample) => isInt(sample?.writtenAtMs)).map((sample) => sample.writtenAtMs).sort((a, b) => a - b);
    for (let i = 1; i < chain.length; i += 1) if (chain[i] - chain[i - 1] > policy.during.maxSnapshotGapMs) problems.push(`during: no broker snapshot for ${chain[i] - chain[i - 1]} ms (limit ${policy.during.maxSnapshotGapMs} ms)`);
  }
  if (!jobs || jobs.checked !== true) problems.push("jobs: the broker job listing was not checked");
  else {
    if (jobs.ours !== 1) problems.push(`jobs: ${jobs.ours} root jobs in the measured checkout match the run, expected exactly 1`);
    if (!Array.isArray(jobs.foreign)) problems.push("jobs: foreign job list is missing");
    else if (jobs.foreign.length) problems.push(`jobs: ${jobs.foreign.length} other root job(s) overlapped the window (${jobs.foreign.slice(0, 3).map((job) => String(job?.id ?? "?")).join(", ")})`);
    const from = before?.cpu?.startedAtMs, to = isNum(after?.cpu?.startedAtMs) && isNum(after?.cpu?.durationMs) ? after.cpu.startedAtMs + after.cpu.durationMs : undefined;
    if (!isNum(jobs.fromMs) || !isNum(jobs.toMs) || !isNum(from) || !isNum(to) || jobs.fromMs > from + policy.skewMs || jobs.toMs < to - policy.skewMs) problems.push("jobs: the job listing does not span the whole window");
    if (!isNum(jobs.checkedAtMs) || jobs.checkedAtMs < (jobs.toMs ?? Infinity) - policy.skewMs) problems.push("jobs: the job listing was read before the window ended");
    // The listing at grant and at release: both are recorded, each at its own moment, and nothing is live at either.
    const listing = (label, row) => {
      if (!row || typeof row !== "object" || row.checked !== true || !isNum(row.checkedAtMs) || !Array.isArray(row.live) || !Array.isArray(row.ids)) { problems.push(`jobs: the broker job listing at ${label} was not recorded`); return null; }
      if (row.live.length) problems.push(`jobs: ${row.live.length} root job(s) were live at ${label} (${row.live.slice(0, 3).map(String).join(", ")})`);
      return row;
    };
    const grant = listing("grant", jobs.atGrant), release = listing("release", jobs.atRelease);
    const beforeEnd = isNum(before?.cpu?.startedAtMs) && isNum(before?.cpu?.durationMs) ? before.cpu.startedAtMs + before.cpu.durationMs : undefined;
    if (grant && (!isNum(beforeEnd) || grant.checkedAtMs < beforeEnd - policy.skewMs || grant.checkedAtMs > start + policy.skewMs || start - grant.checkedAtMs > policy.before.maxStartLagMs)) problems.push("jobs: the job listing at grant was not taken between the quiet check and the run start");
    if (release && (release.checkedAtMs < end - policy.skewMs || !isNum(to) || release.checkedAtMs < to - policy.skewMs)) problems.push("jobs: the job listing at release was not taken after the run ended and its after-sample");
  }
  return problems;
}

/** How close a record is to its ceilings: the largest value/ceiling over the gated figures (delta basis honoured). */
export function worstScore(record, budget) {
  let score = 0;
  for (const key of FIGURES) {
    const delta = key === "wakeupsPerSecond" && budget?.ceilingBasis?.[key] === "delta-over-bare";
    const value = delta ? record?.delta?.[key] : record?.observed?.[key];
    const ceiling = delta ? budget?.deltaCeilings?.[key] : budget?.ceilings?.[key];
    if (isNum(value) && isNum(ceiling) && ceiling > 0) score = Math.max(score, value / ceiling);
  }
  return score;
}
/** Index of the worst repeat (first on a tie). The producer submits it; the verifier recomputes it. */
export const worstIndex = (records, budget) => records.reduce((best, record, index) => (worstScore(record, budget) > worstScore(records[best], budget) ? index : best), 0);
