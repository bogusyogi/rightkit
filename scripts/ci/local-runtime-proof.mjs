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

export const WINDOW_POLICY = Object.freeze({
  id: "local-runtime-window/1",
  // Every workload is measured this many times, each in its own quiet window; the WORST run is the submitted record and
  // every run must pass the full budget gate. One lucky quiet run therefore cannot hide a noisy workload.
  minRepeats: 3,
  maxRepeats: 9,
  // The broker rewrites health.json every 5 s; a snapshot older than this is not evidence of the present.
  maxHealthAgeMs: 20_000,
  before: Object.freeze({ minCpuSampleMs: 10_000, minCpuIdlePercent: 90, maxStartLagMs: 120_000 }),
  during: Object.freeze({ maxInFlight: 1, maxAdmitting: 1, maxSnapshotGapMs: 15_000 }),
  after: Object.freeze({ minCpuSampleMs: 5_000, minCpuIdlePercent: 90 }),
  // Slack between two clocks read in one process (sample start versus run start).
  skewMs: 1_000,
});

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

/** Every reason the recorded window is not a quiet one. Empty means quiet. Recomputed from the samples, never from `quiet`. */
export function windowProblems(window, policy = WINDOW_POLICY) {
  if (!window || typeof window !== "object" || Array.isArray(window)) return ["window evidence is missing"];
  const problems = [];
  if (window.policy !== policy.id) problems.push(`window policy ${JSON.stringify(window.policy)} is not ${policy.id}`);
  const { startedAtMs: start, endedAtMs: end, before, during, after, jobs } = window;
  if (!isNum(start) || !isNum(end) || !(end > start)) return [...problems, "window start/end are invalid"];
  problems.push(...idleProblems(before?.health, "before", policy));
  if (before?.health && isInt(before.health.sampledAtMs) && (before.health.sampledAtMs > start + policy.skewMs || start - before.health.sampledAtMs > policy.before.maxStartLagMs)) problems.push("before: the broker snapshot is not immediately before the run");
  problems.push(...cpuProblems(before?.cpu, "before", policy.before));
  if (isNum(before?.cpu?.startedAtMs) && isNum(before?.cpu?.durationMs) && before.cpu.startedAtMs + before.cpu.durationMs > start + policy.skewMs) problems.push("before: the host CPU sample overlaps the run");
  problems.push(...idleProblems(after?.health, "after", policy));
  if (after?.health && isInt(after.health.writtenAtMs) && after.health.writtenAtMs < end) problems.push("after: the broker idle snapshot was written before the run ended");
  problems.push(...cpuProblems(after?.cpu, "after", policy.after));
  if (isNum(after?.cpu?.startedAtMs) && after.cpu.startedAtMs < end - policy.skewMs) problems.push("after: the host CPU sample overlaps the run");
  const samples = Array.isArray(during?.health) ? during.health : null;
  if (!samples) problems.push("during: no broker snapshots were recorded");
  else {
    const seen = new Map();
    for (const [index, sample] of samples.entries()) {
      const label = `during[${index}]`;
      const bad = snapshotProblems(sample, label, policy);
      problems.push(...bad);
      if (bad.length) continue;
      if (sample.inFlight > policy.during.maxInFlight || sample.admitting > policy.during.maxAdmitting || sample.queued !== 0) problems.push(`${label}: broker shows inFlight ${sample.inFlight}, queued ${sample.queued}, admitting ${sample.admitting}; only the measured job may run`);
      if (sample.sampledAtMs < start - policy.skewMs || sample.sampledAtMs > end + policy.skewMs) problems.push(`${label}: snapshot was read outside the run`);
      seen.set(sample.writtenAtMs, sample);
    }
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
