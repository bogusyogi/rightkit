// Shared compile planning: preserve every publisher requirement while coalescing only provably identical
// invocations or compatible cargo check/clippy requests. Execution is supplied by the existing snapshot scan runner.
import { createHash } from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { scanCrateForSecrets } from "./publish-cargo.mjs";

const digest = (value) => createHash("sha256").update(JSON.stringify(value)).digest("hex");
const bodyOf = ({ workspace, cwd, executable, internal, argv, env, phase }) => ({ workspace, cwd, executable, ...(internal === undefined ? {} : { internal }), argv, env, phase });

// NN27 (GATE-CONTRACT GC-A27): never two compiler passes over one crate, target and feature set. A publisher `cargo check` is proved by a
// passing plan row (clippy or check) over the same crate, features, target and all-targets selection with the same compiler-relevant
// environment: clippy runs the same compile and only adds lints. Output and failure flags (--message-format, --locked, --keep-going,
// -j, lint arguments after `--`) do not change what compiled. Tests and docs never reuse: their environment or target selection differs.
const COMPILE_ENV = ["CC", "CXX", "AR", "LD", "RUSTC", "RUSTFLAGS", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER", "CARGO_TARGET_DIR", "CARGO_BUILD_TARGET", "MACOSX_DEPLOYMENT_TARGET", "CARGO_ENCODED_RUSTFLAGS"];
/** `as`: "check" (what a check or clippy proves) or "clippy" (a clippy with its lint arguments, provable only by the same clippy). */
export function compileProofKey(scan, as = "check") {
  if (!scan || !Array.isArray(scan.args)) return null;
  const argv = scan.args[0] === "cargo" ? scan.args.slice(1) : scan.args;
  const sub = argv[0];
  if (sub !== "check" && sub !== "clippy") return null;
  if (as === "clippy" && sub !== "clippy") return null;
  const sep = argv.indexOf("--");
  const lints = as === "clippy" ? (sep < 0 ? [] : argv.slice(sep + 1)) : null;
  const packages = []; const features = []; let noDefault = false; let allFeatures = false; let target = null; let allTargets = false;
  for (let i = 1; i < argv.length; i += 1) {
    const a = argv[i];
    if (a === "--") break;
    if (a === "-p" || a === "--package") packages.push(argv[++i]);
    else if (a === "--features" || a === "-F") features.push(...String(argv[++i] ?? "").split(/[ ,]+/).filter(Boolean));
    else if (a.startsWith("--features=")) features.push(...a.slice(11).split(/[ ,]+/).filter(Boolean));
    else if (a === "--no-default-features") noDefault = true;
    else if (a === "--all-features") allFeatures = true;
    else if (a === "--target") target = argv[++i];
    else if (a === "--all-targets") allTargets = true;
    else if (a === "--locked" || a === "--keep-going" || a === "--no-deps" || a.startsWith("--message-format") || a === "--profile" || a.startsWith("--profile=") || a === "-j" || a === "--jobs" || a.startsWith("-j") || a.startsWith("--jobs=")) { if (a === "--profile" || a === "-j" || a === "--jobs") i += 1; }
    else return null; // anything else could change what compiles: no proof either way
  }
  if (packages.length !== 1 || !packages[0]) return null;
  const env = Object.fromEntries(COMPILE_ENV.map((k) => [k, scan.env?.[k] ?? null]));
  return JSON.stringify({ as, ...(lints ? { lints } : {}), cwd: scan.cwd ?? ".", package: packages[0], features: [...new Set(features)].sort(), noDefault, allFeatures, target, allTargets, env });
}

/** One in-memory plan; env is execution-only and must never be written to a report. */
export function planUnitListCompile(requirementsByUnit, { normalizedCwds = new Map() } = {}) {
  if (!Array.isArray(requirementsByUnit) || !requirementsByUnit.length) throw new Error("unit list needs compile requirements");
  const ids = new Set(); const exact = new Map();
  for (const entry of requirementsByUnit) {
    if (typeof entry?.selector !== "string" || !Array.isArray(entry.requirements) || !entry.requirements.length) throw new Error("each unit needs a nonempty canonical compile plan");
    for (const [order, r] of entry.requirements.entries()) {
      const body = bodyOf(r);
      if (typeof r.id !== "string" || ids.has(r.id) || typeof r.workspace !== "string" || !r.workspace || typeof r.cwd !== "string" || !r.cwd || (typeof r.executable !== "string" && !(r.executable === null && r.internal === "scanCrateForSecrets")) || !Array.isArray(r.argv) || !r.argv.every((x) => typeof x === "string") || !r.env || typeof r.env !== "object" || Array.isArray(r.env) || !["test", "compile", "check"].includes(r.phase) || digest(body) !== r.coverageKey) throw new Error(`invalid or duplicate canonical requirement ${r.id ?? "<missing>"}`);
      ids.add(r.id);
      const member = { selector: entry.selector, id: r.id, coverageKey: r.coverageKey, order };
      if (exact.has(r.coverageKey)) exact.get(r.coverageKey).members.push(member);
      else exact.set(r.coverageKey, { ...body, cwd: normalizedCwds.get(r.id) ?? body.cwd, members: [member] });
    }
  }
  const groups = new Map(); const jobs = [];
  for (const job of exact.values()) {
    const at = job.argv.indexOf("-p");
    const crate = at >= 0 ? job.argv[at + 1] : undefined;
    const rest = at < 0 ? [] : [...job.argv.slice(0, at), ...job.argv.slice(at + 2)];
    // Tests, Cargo features, and commands with multiple selectors never unify. Doc rows do (rustdoc is single-threaded per crate: one crate per job left the cores idle, ~3 h for 340 rows; Mac 2026-11-07).
    const batchable = job.executable === "cargo" && job.phase === "compile" && ["check", "clippy", "doc"].includes(job.argv[0]) && at > 0 && typeof crate === "string" && !crate.startsWith("-") && job.argv.indexOf("-p", at + 1) < 0 && !rest.some((x) => /^(?:--features(?:=|$)|-F)/.test(x));
    if (!batchable) { jobs.push(job); continue; }
    for (const member of job.members) member.package = crate;
    const key = JSON.stringify({ workspace: job.workspace, cwd: job.cwd, executable: job.executable, env: job.env, phase: job.phase, rest, at });
    let group = groups.get(key);
    if (!group) { group = { ...job, argv: [...job.argv], members: [...job.members], packages: [crate], at }; groups.set(key, group); jobs.push(group); }
    else if (!group.packages.includes(crate)) { group.packages.push(crate); group.members.push(...job.members); }
    else group.members.push(...job.members);
  }
  return jobs.map((job) => {
    if (!job.packages) return job;
    const { at, packages, ...rest } = job;
    return { ...rest, batchAt: at, argv: [...job.argv.slice(0, at), ...packages.flatMap((name) => ["-p", name]), ...job.argv.slice(at + 2)] };
  });
}

/** Prove that moving a narrow package-selected Cargo invocation to its workspace root preserves discovery. */
export function proveWorkspaceCwd({ requirement: r, snapshot, checkout, toolchainOf, exists = fs.existsSync }) {
  const no = (reason) => ({ cwd: r.cwd, normalized: false, reason });
  if (r.executable !== "cargo" || r.phase !== "compile" || !["check", "clippy"].includes(r.argv[0]) || r.workspace === r.cwd) return no("not a movable Cargo check or clippy");
  const workspace = path.resolve(snapshot, r.workspace); const cwd = path.resolve(snapshot, r.cwd);
  const relative = path.relative(workspace, cwd);
  if (!relative || relative.startsWith("..") || path.isAbsolute(relative)) return no("working directory is outside workspace");
  // Only the publisher's package-selected, path-free shape can move. Cargo's configuration and toolchain
  // lookups depend on cwd; inspect every intermediate directory in both trees, including crate itself.
  const args = r.argv; let packages = 0;
  for (let i = 1; i < args.length; i += 1) {
    const a = args[i];
    if (a === "-p") { if (!/^[a-zA-Z0-9_-]+$/.test(args[++i] ?? "")) return no("invalid package selector"); packages += 1; }
    else if (["--target", "--jobs", "--profile", "-D"].includes(a)) { if (!/^[a-zA-Z0-9_.-]+$/.test(args[++i] ?? "")) return no("cwd-sensitive argument"); }
    else if (!["--", "--no-deps", "--all-targets", "--all-features", "--no-default-features", "--locked", "--frozen", "--offline", "--message-format=json-render-diagnostics"].includes(a)) return no(`unproven argument ${a}`);
  }
  if (packages !== 1) return no("requires exactly one package selector");
  for (const [key, value] of Object.entries(r.env)) {
    if (typeof value !== "string") return no(`non-string environment ${key}`);
    if (/^(?:PATH|Path)$/.test(key) && value.split(path.delimiter).some((part) => !path.isAbsolute(part))) return no(`cwd-sensitive environment ${key}`);
    if (/^(?:RUSTFLAGS|RUSTDOCFLAGS|CARGO_ENCODED_RUSTFLAGS|CARGO_TARGET_.*_RUSTFLAGS)$/.test(key) && value && !/^(?:-D\s*warnings|-A\s*warnings)$/.test(value.trim())) return no(`unproven compiler flags in ${key}`);
    if (/^(?:CARGO_|RUST|CC(?:_|$)|CXX(?:_|$)|AR(?:_|$)|PKG_CONFIG)/.test(key) && (/[\\/]/.test(value) && !path.isAbsolute(value) || /(?:^|\s)\.{1,2}[\\/]/.test(value))) return no(`cwd-sensitive environment ${key}`);
  }
  for (const root of [snapshot, checkout]) {
    let at = path.resolve(root, r.workspace);
    for (const part of relative.split(path.sep)) {
      at = path.join(at, part);
      for (const file of [".cargo/config", ".cargo/config.toml", "rust-toolchain", "rust-toolchain.toml"]) if (exists(path.join(at, file))) return no(`cwd override ${path.relative(root, path.join(at, file))}`);
    }
  }
  if (typeof toolchainOf !== "function") return no("effective toolchain is unverified");
  let toolchains;
  try { toolchains = [snapshot, checkout].flatMap((root) => [r.workspace, r.cwd].map((dir) => toolchainOf(path.resolve(root, dir)))); } catch { return no("effective toolchain probe failed"); }
  const locations = ["snapshot workspace", "snapshot unit", "checkout workspace", "checkout unit"];
  for (const [i, probe] of toolchains.entries()) if (!(typeof probe === "string" ? probe : probe?.toolchain)) return no(`effective toolchain unavailable at ${locations[i]} (${typeof probe?.error === "string" && /^(?:spawn-error|exit-(?:\d+|unknown))$/.test(probe.error) ? probe.error : "empty-result"})`);
  if (new Set(toolchains.map((probe) => typeof probe === "string" ? probe : probe.toolchain)).size !== 1) return no("effective toolchain differs across workspace and unit");
  return { cwd: r.workspace, normalized: true, reason: "matching Cargo config and effective toolchain" };
}

/** Run the publisher's own file-only crate scan against the guarded snapshot. */
export async function runPublisherSecretRequirement({ requirement, snapshot, commit, tree, verifySnapshot }) {
  if (requirement?.internal !== "scanCrateForSecrets" || requirement.phase !== "check" || requirement.executable !== null || requirement.argv?.length !== 1 || requirement.argv[0] !== requirement.cwd || digest(bodyOf(requirement)) !== requirement.coverageKey || !/^crates\/rightkit-[a-z0-9-]+$/.test(requirement.cwd) || typeof verifySnapshot !== "function" || !/^[0-9a-f]{40,64}$/.test(commit ?? "") || !/^[0-9a-f]{40,64}$/.test(tree ?? "")) throw new Error("invalid publisher secret scan or snapshot binding");
  const root = fs.realpathSync(snapshot); const directory = fs.realpathSync(path.join(root, requirement.cwd));
  if (!directory.startsWith(`${root}${path.sep}`)) throw new Error("crate secret scan escapes the snapshot");
  if (await verifySnapshot() !== true) throw new Error("snapshot verification failed before secret scan");
  const findings = scanCrateForSecrets(directory);
  if (await verifySnapshot() !== true) throw new Error("snapshot verification failed after secret scan");
  const evidence = { id: requirement.id, coverageKey: requirement.coverageKey, commit, tree, status: findings.length ? "fail" : "pass", findings };
  return { ...evidence, evidenceHash: digest(evidence) };
}

/**
 * Serial admission keeps at most one Cargo command per workspace in flight and never floods the broker.
 * `run` uses the real scoped snapshot scan runner; `verify` must validate its receipt and command binding.
 * Neither execution environment nor raw receipt is placed in the returned report.
 */
export async function executeUnitListCompile({ jobs, dependencies = {}, initialFailures = new Set(), run, verify, concurrency = 1, lookupOnly = false }) {
  if (!Array.isArray(jobs) || typeof run !== "function" || typeof verify !== "function") throw new Error("snapshot scan runner and receipt verifier are required");
  const pending = jobs.map((job) => ({ ...job, members: job.members.map((member) => ({ ...member })) }));
  const all = pending.flatMap((job) => job.members);
  const expected = new Map();
  for (const member of all) expected.set(member.selector, (expected.get(member.selector) ?? 0) + 1);
  const guardBlocked = new Set(initialFailures);
  let added = true;
  while (added) {
    added = false;
    for (const [selector, deps] of Object.entries(dependencies)) if (!guardBlocked.has(selector) && deps.some((dep) => guardBlocked.has(dep))) { guardBlocked.add(selector); added = true; }
  }
  const done = new Map(); const results = [];
  const record = (member, status, evidenceHash) => {
    const rows = done.get(member.selector) ?? new Map(); rows.set(member.order, status); done.set(member.selector, rows);
    results.push({ selector: member.selector, id: member.id, coverageKey: member.coverageKey, status, ...(evidenceHash ? { evidenceHash } : {}) });
  };
  while (pending.some((job) => job.members.length)) {
    let progress = false;
    // Decide readiness for every unit before executing anything in this round. Otherwise visiting
    // one unit's test first starts its share of a batch before another unit's test has even run.
    for (const job of pending) for (const member of [...job.members]) {
      const dependencyFailed = (dependencies[member.selector] ?? []).some((dep) => {
        const rows = done.get(dep);
        return expected.has(dep) && rows?.size === expected.get(dep) && [...rows.values()].some((status) => status !== "passed");
      });
      if (guardBlocked.has(member.selector) || dependencyFailed) {
        record(member, "blocked"); job.members.splice(job.members.indexOf(member), 1); progress = true;
      }
    }
    const frontier = [];
    for (const job of pending) {
      const active = [];
      for (const member of job.members) {
        const own = done.get(member.selector) ?? new Map();
        const deps = dependencies[member.selector] ?? [];
        const previousReady = Array.from({ length: member.order }, (_, i) => own.has(i)).every(Boolean);
        const depsReady = deps.every((dep) => expected.has(dep) && (done.get(dep)?.size ?? 0) === expected.get(dep));
        if (previousReady && depsReady) active.push(member);
      }
      if (!active.length) continue;
      // Two identical requirements of one unit are never executed out of sequence.
      const selectors = new Set(); const ready = active.filter((m) => !selectors.has(m.selector) && selectors.add(m.selector));
      frontier.push({ job, ready });
    }
    // Prefer a whole ready group, waiting for members whose earlier independent requirements are
    // themselves runnable. If such waits form a cycle, execute the ready subset to make progress.
    let admitted = frontier.filter(({ job, ready }) => ready.length === job.members.length);
    if (!admitted.length && frontier.length && !progress) admitted = [frontier[0]];
    // NN17/NN39: the admitted jobs are independent (one ready requirement per unit, its earlier ones done), so they run `concurrency` at a time;
    // 1 is the serial order this loop had. Every result is still verified per member before anything later is admitted.
    const runAdmitted = async ({ job, ready }) => {
      let invocation = { ...job, members: ready };
      if (job.batchAt !== undefined) {
        const names = [...new Set(ready.map((m) => m.package))];
        const at = job.batchAt; let end = at;
        while (job.argv[end] === "-p" && typeof job.argv[end + 1] === "string") end += 2;
        invocation = { ...invocation, argv: [...job.argv.slice(0, at), ...names.flatMap((name) => ["-p", name]), ...job.argv.slice(end)] };
      }
      let result;
      try { result = await run(invocation); } catch { result = null; }
      for (const member of ready) {
        let status = "failed"; let proof = null;
        const looked = result?.byMember instanceof Map ? result.byMember.get(member.id) : result;
        // A cache lookup proves nothing: it only lets the lookup reach the rows behind this one (no verify, nothing recorded as proof).
        if (lookupOnly && (looked?.status === "pass" || (looked?.status === "not-run" && (looked.lookupMiss === true || looked.uncacheable === true)))) { record(member, "passed", null); job.members.splice(job.members.indexOf(member), 1); continue; }
        try {
          const memberResult = looked;
          const checked = await verify({ job: invocation, result: memberResult, member });
          if (memberResult?.status === "pass" && checked?.ok === true && typeof checked.evidenceHash === "string" && /^[0-9a-f]{64}$/.test(checked.evidenceHash)) { status = "passed"; proof = checked.evidenceHash; }
        } catch { /* An exception cannot attest a pass. */ }
        record(member, status, proof); job.members.splice(job.members.indexOf(member), 1);
      }
      progress = true;
    };
    const queue = [...admitted];
    await Promise.all(Array.from({ length: Math.max(1, Math.min(Number(concurrency) || 1, queue.length)) }, async () => { while (queue.length) await runAdmitted(queue.shift()); }));
    if (!progress) throw new Error("unit requirements have an unresolved or cyclic dependency");
  }
  return results;
}

/** Bridge the authenticated publisher jobs to runScans' initialized Class B scheduler. */
export function publisherUnionScheduler({ reuseProofs = true, concurrency = 1, originConcurrency = 8, jobs, dependencies = {}, initialFailures = new Set(), requirements, adapt, verify, signal = null, onCoverage = () => {} }) {
  if (!Array.isArray(requirements) || requirements.some((r) => r.internal) || typeof adapt !== "function" || typeof verify !== "function") throw new Error("publisher scheduler needs external requirements, adapter and verifier; internal scans must already be verified");
  const byId = new Map(requirements.map((r) => [r.id, r]));
  if (byId.size !== requirements.length) throw new Error("duplicate publisher requirement IDs");
  const plannedIds = jobs.flatMap((job) => job.members.map((member) => member.id));
  if (plannedIds.length !== requirements.length || new Set(plannedIds).size !== plannedIds.length || plannedIds.some((id) => !byId.has(id))) throw new Error("publisher job coverage differs from authenticated requirements");
  return async ({ scans, execute, executeGroup, canGroup, project, notRun, probe = null, store = null, lookupOnly = false }) => {
    // Lookup only (the gate's cache lookup before a run): every row the real run would execute is looked up. A looked-up row stands for the pass the
    // real run needs before its dependents and NN27 proofs, so a cold host reports every row it would run, not only the first frontier.
    const provedInLookup = new Map(); const provedRows = new Map();
    const wouldPass = (row) => row?.status === "pass" || (lookupOnly && row?.status === "not-run" && (row.lookupMiss === true || row.uncacheable === true));
    const byScan = new Map(scans.map((scan, index) => [scan.id, index]));
    if (byScan.size !== scans.length || requirements.some((r) => !byScan.has(r.id))) throw new Error("publisher plan does not contain each requirement exactly once");
    const slots = new Array(scans.length); const executed = new Map(); const attempts = new Map();
    // A quiet suite needs the broker drained for its quiet lease: a job with a quiet member runs alone (exclusive), the others share.
    let shared = 0; let exclusive = false; const waiters = [];
    const wake = () => { const ready = waiters.splice(0); for (const w of ready) w(); };
    const acquire = async (alone) => {
      for (;;) {
        if (alone ? (!exclusive && shared === 0) : !exclusive) { if (alone) exclusive = true; else shared += 1; return; }
        await new Promise((resolve) => waiters.push(resolve));
      }
    };
    const release = (alone) => { if (alone) exclusive = false; else shared -= 1; wake(); };
    // Origin scans have no publisher per-unit sequencing; batch only members the scan runner itself marks compatible, and retain its signed
    // bisection/attribution results. NN17/NN39 (Mac 2026-11-07: the broker never ran two jobs at once; 30 single rows ran one at a time, 8 rows/min):
    // the groups and single rows are formed first, then submitted `originConcurrency` at a time in plan order; the broker orders and runs them.
    const work = []; const claimed = new Set();
    for (let i = 0; i < scans.length; i += 1) if (!byId.has(scans[i].id) && !claimed.has(i)) {
      const peers = typeof executeGroup === "function" && typeof canGroup === "function" ? scans.map((s, j) => j > i && !claimed.has(j) && !byId.has(s.id) && canGroup(scans[i], s) ? j : -1).filter((j) => j >= 0) : [];
      const indices = [i, ...peers]; for (const j of indices) claimed.add(j); work.push(indices);
    }
    const runWork = async (indices) => {
      const alone = indices.some((j) => scans[j]?.quiet === true);
      await acquire(alone);
      try {
        if (indices.length === 1) slots[indices[0]] = await execute(scans[indices[0]]);
        else { const rows = await executeGroup(indices.map((j) => scans[j])); for (const [n, j] of indices.entries()) slots[j] = rows[n]; }
      } finally { release(alone); }
    };
    const queue = [...work];
    await Promise.all(Array.from({ length: Math.max(1, Math.min(Number(originConcurrency) || 1, queue.length)) }, async () => { while (queue.length) await runWork(queue.shift()); }));
    // Passing plan rows that prove a compile (NN27), with the evidence a publisher verify needs on a broker host.
    const proofs = new Map();
    for (let i = 0; i < scans.length; i += 1) {
      if (byId.has(scans[i].id) || !wouldPass(slots[i])) continue;
      if (!lookupOnly && scans[i].root === "cargoLive" && !/^[0-9a-f]{64}$/.test(slots[i].snapshotEvidence?.receiptSha256 ?? "")) continue;
      for (const as of ["check", "clippy"]) { const key = compileProofKey(scans[i], as); if (key !== null && !proofs.has(key)) proofs.set(key, { id: scans[i].id, row: slots[i] }); }
    }
    const result = await executeUnitListCompile({ concurrency, jobs, dependencies, initialFailures, lookupOnly,
      run: async (job) => {
        const alone = job.members.some((member) => scans[byScan.get(member.id)]?.quiet === true);
        await acquire(alone);
        try { return await runJob(job); } finally { release(alone); }
      },
      verify: async ({ job, result: row, member }) => verify({ job, row, member, attempts: attempts.get(member.id) ?? [], requirement: byId.get(member.id) }),
    });
    async function runJob(job) {
      {
        const byMember = new Map();
        const budget = { remaining: Math.min(2 * job.members.length - 1, 31) };
        const runMembers = async (members) => {
          if (budget.remaining <= 0 || signal?.aborted) {
            for (const member of members) { const history = attempts.get(member.id) ?? []; history.push({ status: "not-run", code: signal?.aborted ? "CANCELLED" : "BATCH_BUDGET" }); attempts.set(member.id, history); }
            return;
          }
          // NN27: a member whose compile a passing plan row already proved reuses that row; only the rest compile.
          const proved = [];
          const rest = members.filter((member) => {
            const own = { ...scans[byScan.get(member.id)], cwd: job.cwd }; // the job's cwd is the workspace when proveWorkspaceCwd moved it
            const ownArgv = own.args?.[0] === "cargo" ? own.args.slice(1) : own.args ?? [];
            const key = compileProofKey(own, ownArgv[0] === "clippy" ? "clippy" : "check");
            const proof = key === null || !reuseProofs ? undefined : proofs.get(key);
            if (!proof) return true;
            proved.push([member, proof]); return false;
          });
          for (const [member, proof] of proved) {
            // In a lookup the proving row may itself be a miss; the member is proved by it, never looked up or run on its own.
            if (lookupOnly) provedInLookup.set(member.id, proof.id);
            provedRows.set(member.id, proof.row); // the executor-issued row: only it can be projected onto the member's slot
            const row = lookupOnly ? { ...proof.row, status: "pass", lookupMiss: false, uncacheable: false, provedBy: proof.id } : { ...proof.row, provedBy: proof.id };
            const history = attempts.get(member.id) ?? [];
            history.push({ id: proof.id, status: row.status, command: row.command, provedBy: proof.id, ...(row.snapshotEvidence ? { snapshotEvidence: row.snapshotEvidence } : {}) });
            attempts.set(member.id, history); executed.set(member.id, row); byMember.set(member.id, row);
          }
          if (!rest.length) return;
          // Per-member cache (rows passed alone or in another grouping stay hits): each remaining member is looked up on its OWN key before any batch is made,
          // so a change in how crates are grouped reruns nothing that passed. A looked-up miss carries what a later per-member store needs.
          const missing = new Map();
          if (typeof probe === "function" && rest.length > 1) {
            const looked = await Promise.all(rest.map(async (member) => [member, await probe(scans[byScan.get(member.id)])]));
            const still = [];
            for (const [member, r] of looked) {
              if (r?.cached === true && r.status === "pass") {
                provedRows.set(member.id, r); // a cached member is its own hit in a lookup too (counted as one), never an unclassified row
                const history = attempts.get(member.id) ?? [];
                history.push({ id: r.id, status: "pass", cached: true, command: r.command, ...(r.snapshotEvidence ? { snapshotEvidence: r.snapshotEvidence } : {}) });
                attempts.set(member.id, history); executed.set(member.id, r); byMember.set(member.id, r);
              } else { if (r?.memberWrite) missing.set(member.id, r.memberWrite); still.push(member); if (lookupOnly && r) { executed.set(member.id, r); byMember.set(member.id, r); const h = attempts.get(member.id) ?? []; h.push({ id: r.id, status: r.status, command: r.command }); attempts.set(member.id, h); } }
            }
            if (lookupOnly) return; // a lookup reports each member on its own row; no batch is looked up
            if (!still.length) return;
            members = still;
          } else members = rest;
          budget.remaining -= 1;
          const representative = byId.get(members[0].id);
          if (!representative) throw new Error("unknown publisher requirement");
          let argv = job.argv;
          if (job.batchAt !== undefined) {
            const at = job.batchAt; let end = at;
            while (argv[end] === "-p" && typeof argv[end + 1] === "string") end += 2;
            argv = [...argv.slice(0, at), ...[...new Set(members.map((m) => m.package))].flatMap((name) => ["-p", name]), ...argv.slice(end)];
          }
          const one = members.length === 1 && JSON.stringify(argv) === JSON.stringify(representative.argv);
          const scan = one ? scans[byScan.get(representative.id)] : adapt({ ...representative, id: `publisher-batch:${digest(members.map((m) => m.id)).slice(0, 24)}`, cwd: job.cwd, argv, coverageKey: digest(members.map((m) => m.coverageKey)) });
          const row = await execute(scan);
          for (const member of members) {
            const history = attempts.get(member.id) ?? [];
            history.push({ id: row.id, status: row.status, ...(row.code ? { code: row.code } : {}), command: row.command, ...(row.authenticatedFailure ? { authenticatedFailure: row.authenticatedFailure } : {}), ...(row.snapshotEvidence ? { snapshotEvidence: row.snapshotEvidence } : {}) });
            attempts.set(member.id, history);
          }
          const attributable = (row.code === "EXIT" && !row.snapshotEvidence) || (row.code === "BROKER_COMPILE_FAILURE" && row.authenticatedFailure?.retryable === true && row.authenticatedFailure?.failureClass === "compile" && row.snapshotEvidence?.receiptSha256);
          if (members.length > 1 && job.batchAt !== undefined && row.status === "fail" && attributable) {
            const half = Math.ceil(members.length / 2);
            await runMembers(members.slice(0, half)); await runMembers(members.slice(half));
          }
          // Split outcomes attribute the failure; they cannot turn the required combined
          // invocation into a pass or unblock a selected dependent.
          for (const member of members) { executed.set(member.id, row); byMember.set(member.id, row); }
          // A member that passed inside a batch is stored under its own key (per-member cache above), as if it had run alone.
          if (typeof store === "function" && members.length > 1 && row.status === "pass") for (const member of members) store(missing.get(member.id) ?? null, { ...row, id: member.id, command: scans[byScan.get(member.id)]?.command ?? row.command });
        };
        await runMembers(job.members);
        return { byMember };
      }
    }
    // A lookup's "passed" only lets the lookup reach later rows: it is never coverage evidence.
    if (lookupOnly) {
      for (const item of result) {
        const index = byScan.get(item.id); const scan = scans[index]; const row = executed.get(item.id);
        slots[index] = provedInLookup.has(item.id) ? notRun(scan, `proved by ${provedInLookup.get(item.id)} (lookup)`)
          : row ? project(row, scan) : notRun(scan, item.status === "blocked" ? "a prerequisite did not pass" : "not looked up");
      }
      return slots;
    }
    onCoverage(result);
    for (const item of result) {
      const index = byScan.get(item.id); const scan = scans[index]; const row = executed.get(item.id);
      slots[index] = !row || (item.status !== "passed" && row.status === "pass")
        ? notRun(scan, item.status === "blocked" ? "a prerequisite did not pass" : "publisher evidence did not verify")
        : project(provedRows.get(item.id) ?? row, scan);
    }
    return slots;
  };
}

/** Build publisher requirements from one verified snapshot and its Cargo/npm discovery results. */
export async function publisherRequirementsForUnits({ selected, snapshot, rustUnits, npmUnits, platform, env }) {
  if (!Array.isArray(selected) || !Array.isArray(rustUnits) || !Array.isArray(npmUnits)) throw new Error("verified unit discovery is required");
  const readJson = (file) => JSON.parse(fs.readFileSync(path.join(snapshot, file), "utf8"));
  const config = readJson("ci/gate.config.json");
  const assetGated = readJson("ci/asset-gated.json");
  const { compileRequirements } = await import("./local-publish.mjs");
  const out = [];
  for (const row of selected) {
    const cargo = /^cargo:(rightkit-[a-z0-9-]+)$/.exec(row.selector);
    const npm = /^npm:(@rightkit\/[a-z0-9-]+)$/.exec(row.selector);
    if (!cargo && !npm) throw new Error(`unsupported selector ${row.selector}`);
    const kind = cargo ? "cargo" : "npm"; const unit = (cargo ?? npm)[1];
    const dir = cargo ? `crates/${unit}` : `packages/${unit.slice("@rightkit/".length)}`;
    if (row.dir !== dir) throw new Error(`${row.selector}: selected directory differs from canonical directory`);
    const discovered = (cargo ? rustUnits : npmUnits).find((u) => u.dir === dir);
    if (!discovered) throw new Error(`${row.selector}: selected unit is absent from authenticated workspace discovery`);
    if (cargo && (discovered.crate ?? discovered.name) !== unit) throw new Error(`${row.selector}: discovered Cargo identity differs`);
    const manifest = npm ? readJson(`${dir}/package.json`) : null;
    if (npm && manifest?.name !== unit) throw new Error(`${row.selector}: npm manifest identity differs`);
    if (npm && manifest.private === true && !manifest.scripts?.build && !manifest.scripts?.test) throw new Error(`${row.selector}: private unit has no own build or test script; recursive workspace build cannot prove its coverage`);
    // Class A's full pass (rustfmt over every Rust file of this snapshot) precedes these requirements in the same run: no per-crate fmt (GC-A24).
    const planned = compileRequirements({ kind, unit, dir, config, manifest, platform, workspace: cargo ? discovered.cargoDir : ".", env, assetGated, formatProven: true });
    out.push({ selector: row.selector, dir, ...planned });
  }
  return out;
}
