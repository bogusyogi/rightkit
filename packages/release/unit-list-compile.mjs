// Shared compile planning: preserve every publisher requirement while coalescing only provably identical
// invocations or compatible cargo check/clippy requests. Execution is supplied by the existing snapshot scan runner.
import { createHash } from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { scanCrateForSecrets } from "./publish-cargo.mjs";

const digest = (value) => createHash("sha256").update(JSON.stringify(value)).digest("hex");
const bodyOf = ({ workspace, cwd, executable, internal, argv, env, phase }) => ({ workspace, cwd, executable, ...(internal === undefined ? {} : { internal }), argv, env, phase });

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
    // Tests, docs, Cargo features, and commands with multiple selectors never unify.
    const batchable = job.executable === "cargo" && job.phase === "compile" && ["check", "clippy"].includes(job.argv[0]) && at > 0 && typeof crate === "string" && !crate.startsWith("-") && job.argv.indexOf("-p", at + 1) < 0 && !rest.some((x) => /^(?:--features(?:=|$)|-F)/.test(x));
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
export async function executeUnitListCompile({ jobs, dependencies = {}, initialFailures = new Set(), run, verify }) {
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
    for (const { job, ready } of admitted) {
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
        try {
          const memberResult = result?.byMember instanceof Map ? result.byMember.get(member.id) : result;
          const checked = await verify({ job: invocation, result: memberResult, member });
          if (memberResult?.status === "pass" && checked?.ok === true && typeof checked.evidenceHash === "string" && /^[0-9a-f]{64}$/.test(checked.evidenceHash)) { status = "passed"; proof = checked.evidenceHash; }
        } catch { /* An exception cannot attest a pass. */ }
        record(member, status, proof); job.members.splice(job.members.indexOf(member), 1);
      }
      progress = true;
    }
    if (!progress) throw new Error("unit requirements have an unresolved or cyclic dependency");
  }
  return results;
}

/** Bridge the authenticated publisher jobs to runScans' initialized Class B scheduler. */
export function publisherUnionScheduler({ jobs, dependencies = {}, initialFailures = new Set(), requirements, adapt, verify, signal = null, onCoverage = () => {} }) {
  if (!Array.isArray(requirements) || requirements.some((r) => r.internal) || typeof adapt !== "function" || typeof verify !== "function") throw new Error("publisher scheduler needs external requirements, adapter and verifier; internal scans must already be verified");
  const byId = new Map(requirements.map((r) => [r.id, r]));
  if (byId.size !== requirements.length) throw new Error("duplicate publisher requirement IDs");
  const plannedIds = jobs.flatMap((job) => job.members.map((member) => member.id));
  if (plannedIds.length !== requirements.length || new Set(plannedIds).size !== plannedIds.length || plannedIds.some((id) => !byId.has(id))) throw new Error("publisher job coverage differs from authenticated requirements");
  return async ({ scans, execute, executeGroup, canGroup, project, notRun }) => {
    const byScan = new Map(scans.map((scan, index) => [scan.id, index]));
    if (byScan.size !== scans.length || requirements.some((r) => !byScan.has(r.id))) throw new Error("publisher plan does not contain each requirement exactly once");
    const slots = new Array(scans.length); const executed = new Map(); const attempts = new Map();
    // Origin scans have no publisher per-unit sequencing; batch only members the scan runner
    // itself marks compatible, and retain its signed bisection/attribution results.
    for (let i = 0; i < scans.length; i += 1) if (!byId.has(scans[i].id) && !slots[i]) {
      const peers = typeof executeGroup === "function" && typeof canGroup === "function" ? scans.map((s, j) => j > i && !slots[j] && !byId.has(s.id) && canGroup(scans[i], s) ? j : -1).filter((j) => j >= 0) : [];
      if (!peers.length) slots[i] = await execute(scans[i]);
      else {
        const indices = [i, ...peers]; const rows = await executeGroup(indices.map((j) => scans[j]));
        for (const [n, j] of indices.entries()) slots[j] = rows[n];
      }
    }
    const result = await executeUnitListCompile({ jobs, dependencies, initialFailures,
      run: async (job) => {
        const byMember = new Map();
        const budget = { remaining: Math.min(2 * job.members.length - 1, 31) };
        const runMembers = async (members) => {
          if (budget.remaining <= 0 || signal?.aborted) {
            for (const member of members) { const history = attempts.get(member.id) ?? []; history.push({ status: "not-run", code: signal?.aborted ? "CANCELLED" : "BATCH_BUDGET" }); attempts.set(member.id, history); }
            return;
          }
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
        };
        await runMembers(job.members);
        return { byMember };
      },
      verify: async ({ job, result: row, member }) => verify({ job, row, member, attempts: attempts.get(member.id) ?? [], requirement: byId.get(member.id) }),
    });
    onCoverage(result);
    for (const item of result) {
      const index = byScan.get(item.id); const scan = scans[index]; const row = executed.get(item.id);
      slots[index] = !row || (item.status !== "passed" && row.status === "pass")
        ? notRun(scan, item.status === "blocked" ? "a prerequisite did not pass" : "publisher evidence did not verify")
        : project(row, scan);
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
    const planned = compileRequirements({ kind, unit, dir, config, manifest, platform, workspace: cargo ? discovered.cargoDir : ".", env, assetGated });
    out.push({ selector: row.selector, dir, ...planned });
  }
  return out;
}
