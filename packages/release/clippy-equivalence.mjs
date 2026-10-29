// Batch equivalence for the Clippy route (GATE-CONTRACT §1 "Batching", E14; item 8 rows 8.7 and 8.16). A batched Clippy invocation
// replaces the per-crate ones only when it is shown to say the same thing: the same diagnostics (package, target, code, span), the
// same resolved features for what it compiled (feature unification across the selected packages can change what compiles), and the
// same engine and toolchain. This module is the comparison; `run` below drives real cargo over a checkout (a gate-owned export, a
// Linux host) and writes the recorded comparison.
//
//   node clippy-equivalence.mjs --repo <dir> --out <dir> [--units a,b,...] [--target-dir <dir>] [--cargo cargo]
//
// Exit 0 when every batch is equivalent to its per-crate runs (after any unified crate was split out and the split re-run), 1 when not.
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { spawnSync } from "./background-process.mjs";
import { fileURLToPath } from "node:url";
import { clippyArgs, clippyBatchPlan, sidecarBoundCrate } from "./deterministic-scans.mjs";
import { rustCratePlan } from "./scan-digest.mjs";
import { parseCargoMessages, summarizeMessages, toolchainIdentity } from "./clippy-route.mjs";

export const EQUIVALENCE_SCHEMA = "rightkit.clippy-batch-equivalence/1";
const sha256 = (v) => crypto.createHash("sha256").update(v).digest("hex");

/** One diagnostic's identity for the comparison: package, target, level, code (else its first line) and primary span. */
export const diagnosticKey = (d) => JSON.stringify([d.package, d.kind, d.target, d.level, d.code ?? String(d.message).split("\n")[0], d.span ? [d.span.file, d.span.line, d.span.column, d.span.lineEnd, d.span.columnEnd] : null]);
const artifactKey = (a) => JSON.stringify([a.package, a.kind, a.target, a.test === true]);
const multiset = (keys) => { const m = new Map(); for (const k of keys) m.set(k, (m.get(k) ?? 0) + 1); return m; };
const diff = (a, b) => { const out = []; for (const [k, n] of a) { const d = n - (b.get(k) ?? 0); for (let i = 0; i < d; i += 1) out.push(k); } return out.sort(); };

/**
 * Compares each batch with the per-crate runs of its members. `groups`: `[{ id, units, batch: { messages, toolchain }, perCrate:
 * [{ unit, crate, messages, toolchain }] }]` (`messages` are parsed cargo JSON objects; `units` the batch's unit names, which
 * `perCrate` covers one for one). Returns `{ equivalent, groups, separate, failures }`: `separate` maps each unit whose batched
 * compile differed (a different feature set for itself, or a dependency built with features no per-crate run built it with) to the
 * reason, for the planner to keep it as its own invocation; any difference in diagnostics, or a missing or unequal toolchain
 * identity, is a failure of the run (row 8.16).
 */
export function compareEquivalence({ groups }) {
  const failures = []; const separate = new Map(); const rows = [];
  for (const g of groups) {
    const members = new Set(g.perCrate.map((p) => p.crate));
    const batch = summarizeMessages(g.batch.messages);
    const per = g.perCrate.map((p) => ({ ...p, run: summarizeMessages(p.messages) }));
    const row = { id: g.id, units: g.units, crates: [...members].sort() };
    // A member the batch never built (no artifact, no diagnostic) while another member of the batch failed is BLOCKED in the batch: with
    // `-D warnings` a lint is a build failure, so a dependent of a failing member builds nothing under --keep-going where its own per-crate
    // run (which lints only itself) would have run. That is the route's reported `blocked` state (row 8.4), not a clean result, and it is
    // excluded from the comparison; a member the batch did not build while nothing in it failed is a difference.
    const failedInBatch = new Set(batch.diagnostics.filter((d) => members.has(d.package) && d.level.startsWith("error")).map((d) => d.package));
    const builtInBatch = new Set([...batch.artifacts.map((a) => a.package), ...batch.diagnostics.map((d) => d.package)]);
    const blocked = [...members].filter((m) => !builtInBatch.has(m)).sort();
    // Not built in the batch, nothing in it failed, and the member's own per-crate run did build it: a difference. A member neither run
    // builds (a build script that fails on this host, a missing system library) is the same in both.
    const builtOnItsOwn = (m) => per.some((p) => p.crate === m && p.run.artifacts.some((a) => a.package === m));
    const unbuilt = failedInBatch.size === 0 ? blocked.filter(builtOnItsOwn) : [];
    row.blockedInBatch = failedInBatch.size ? blocked : [];
    const compared = (pkg) => members.has(pkg) && !row.blockedInBatch.includes(pkg);
    // Diagnostics: the batch's, for its members, against each per-crate run's, for that crate.
    const batchKeys = multiset(batch.diagnostics.filter((d) => compared(d.package)).map(diagnosticKey));
    const perKeys = multiset(per.flatMap((p) => p.run.diagnostics.filter((d) => d.package === p.crate && compared(p.crate)).map(diagnosticKey)));
    row.diagnostics = { batch: [...batchKeys.values()].reduce((a, n) => a + n, 0), perCrate: [...perKeys.values()].reduce((a, n) => a + n, 0), onlyInBatch: diff(batchKeys, perKeys), onlyInPerCrate: diff(perKeys, batchKeys) };
    row.diagnosticsOfBlockedMembers = per.filter((p) => row.blockedInBatch.includes(p.crate)).reduce((n, p) => n + p.run.diagnostics.filter((d) => d.package === p.crate).length, 0);
    if (row.diagnostics.onlyInBatch.length || row.diagnostics.onlyInPerCrate.length) failures.push(`${g.id}: the batch's diagnostics differ from its per-crate runs (${row.diagnostics.onlyInBatch.length} only in the batch, ${row.diagnostics.onlyInPerCrate.length} only per crate)`);
    const unitOf = new Map(per.map((p) => [p.crate, p.unit]));
    const byDiagnostics = new Set([...row.diagnostics.onlyInBatch, ...row.diagnostics.onlyInPerCrate].map((k) => JSON.parse(k)[0]));
    for (const pkg of byDiagnostics) if (unitOf.has(pkg)) separate.set(unitOf.get(pkg), `batching changed the diagnostics of ${pkg} (item 8 row 8.7): kept as its own invocation`);
    for (const pkg of unbuilt) if (unitOf.has(pkg)) separate.set(unitOf.get(pkg), `the batch never built ${pkg} although nothing in it failed (item 8 row 8.7)`);
    // Resolved features: what each member was built with, in the batch against its own per-crate run, for every unit both built.
    const unified = [];
    for (const p of per) {
      if (!compared(p.crate)) continue;
      const own = (a) => a.package === p.crate;
      const batchOf = new Map(); for (const a of batch.artifacts.filter(own)) batchOf.set(artifactKey(a), a.features);
      const ownOf = new Map(); for (const a of p.run.artifacts.filter(own)) ownOf.set(artifactKey(a), a.features);
      for (const [key, w] of ownOf) {
        const b = batchOf.get(key);
        if (b !== undefined && JSON.stringify(b) !== JSON.stringify(w)) unified.push({ unit: p.unit, crate: p.crate, unitKey: JSON.parse(key), batchFeatures: b, perCrateFeatures: w });
      }
    }
    const seen = new Map();
    for (const p of per) for (const a of p.run.artifacts.filter((x) => !members.has(x.package))) { const k = `${a.packageId}\0${artifactKey(a)}`; if (!seen.has(k)) seen.set(k, new Set()); seen.get(k).add(JSON.stringify(a.features)); }
    const dependencies = [];
    for (const a of batch.artifacts.filter((x) => !members.has(x.package))) {
      const k = `${a.packageId}\0${artifactKey(a)}`;
      const known = seen.get(k);
      if (known && !known.has(JSON.stringify(a.features))) dependencies.push({ package: a.package, packageId: a.packageId, batchFeatures: a.features, perCrateFeatures: [...known].map((s) => JSON.parse(s)) });
    }
    // Third-party packages built with a union of the batch's features are recorded, not judged: cargo's features are additive, and the
    // linted code is the members'. A member built with other features than its own run is what changes what compiles.
    row.features = { unified, dependenciesBuiltWithOtherFeatures: dependencies.length };
    row.dependenciesBuiltWithOtherFeatures = dependencies.map((d) => d.packageId).sort();
    for (const u of unified) separate.set(u.unit, `batching changed the features ${u.crate} compiles with (${u.batchFeatures.join(",") || "none"} in the batch, ${u.perCrateFeatures.join(",") || "none"} on its own): feature unification (item 8 row 8.7)`);
    // A batch that fails to build where every per-crate run builds, and whose failure no diagnostic of a member explains (a build script, a
    // feature conflict): the unified features cannot all be on together (mutually exclusive ones). A failure a member's diagnostic explains
    // is judged by the diagnostics and the unified-feature comparison above.
    const exits = { batch: g.batch.exit ?? null, perCrate: per.map((p) => p.exit ?? null) };
    row.exits = exits;
    const exitDiffers = exits.batch !== null && exits.batch !== 0 && per.length > 0 && per.every((p) => p.exit === 0) && failedInBatch.size === 0;
    if (exitDiffers) for (const p of per) separate.set(p.unit, "the batch fails to build where every per-crate run builds: its features cannot all be on together (item 8 row 8.7)");
    // Toolchain identity (row 8.16): present and equal for the batch and every per-crate run.
    const toolchains = [g.batch.toolchain, ...per.map((p) => p.toolchain)];
    const equal = toolchains.every((t) => typeof t === "string" && t.trim() !== "") && new Set(toolchains).size === 1;
    row.toolchain = { equal, batch: g.batch.toolchain ?? null, perCrate: [...new Set(per.map((p) => p.toolchain ?? null))] };
    if (!equal) failures.push(`${g.id}: the toolchain identity of the batch and its per-crate runs is not one equal, present string (batch ${g.batch.toolchain ?? "missing"}; per crate ${row.toolchain.perCrate.join(", ")})`);
    row.equivalent = !row.diagnostics.onlyInBatch.length && !row.diagnostics.onlyInPerCrate.length && !unified.length && !exitDiffers && !unbuilt.length && equal;
    rows.push(row);
  }
  if (separate.size) failures.push(`${separate.size} unit(s) are kept separate: ${[...separate.keys()].sort().join(", ")}`);
  return { schema: EQUIVALENCE_SCHEMA, equivalent: failures.length === 0, groups: rows, separate, failures };
}

// ---- the run over a checkout (real cargo) ---------------------------------------------------------------------------------------------

function cargoRun({ cargo, args, cwd, env, messagesFile, clean = [] }) {
  // Each run lints its packages afresh: cargo replays the cached diagnostics of a fresh unit, and what it cached depends on which run
  // compiled the unit first, so a comparison over a shared target directory would compare cache history. Cleaning only the packages
  // the run selects keeps the dependencies' compiled outputs warm.
  if (clean.length) {
    const c = spawnSync(cargo, ["clean", ...clean.flatMap((n) => ["-p", n])], { cwd, env, encoding: "utf8" });
    if (c.status !== 0) throw new Error(`clean of ${clean.join(", ")} failed: ${String(c.stderr).slice(-300)}`);
  }
  const fd = fs.openSync(messagesFile, "w");
  const t = Date.now();
  let r;
  try { r = spawnSync(cargo, args, { cwd, env, stdio: ["ignore", fd, "pipe"], encoding: "utf8", maxBuffer: 256 * 1024 * 1024 }); } finally { fs.closeSync(fd); }
  const body = fs.readFileSync(messagesFile, "utf8");
  return { status: r.status, signal: r.signal, stderr: String(r.stderr ?? "").slice(-4000), messages: parseCargoMessages(body).messages, ms: Date.now() - t, bytes: Buffer.byteLength(body), error: r.error?.message ?? null };
}
const withKeepGoing = (args) => { const i = args.indexOf("--"); return [...args.slice(0, i < 0 ? undefined : i), "--keep-going", ...(i < 0 ? [] : args.slice(i))]; };

/**
 * Runs the per-crate and the batched Clippy invocations of `units` (default: every workspace member) over `repo` with the same
 * toolchain and one shared target directory, compares them, and when a batch is not equivalent because of feature unification
 * splits the affected units out and runs and compares again. Writes `<out>/equivalence.json` (without raw messages) and returns it.
 */
export function run({ repo, out, units = null, targetDir = path.join(out, "target"), cargo = "cargo", env = process.env, log = (m) => process.stderr.write(`${m}\n`) }) {
  fs.mkdirSync(out, { recursive: true });
  const runEnv = { ...env, CARGO_TARGET_DIR: targetDir };
  const meta = spawnSync(cargo, ["metadata", "--no-deps", "--format-version", "1", "--locked"], { cwd: repo, env: runEnv, encoding: "utf8", maxBuffer: 256 * 1024 * 1024 });
  if (meta.status !== 0) throw new Error(`cargo metadata failed: ${String(meta.stderr).slice(-400)}`);
  const metadata = JSON.parse(meta.stdout);
  const byId = new Map(metadata.packages.map((p) => [p.id, p]));
  const members = metadata.workspace_members.map((id) => byId.get(id)).filter(Boolean).sort((a, b) => (a.name < b.name ? -1 : 1));
  const config = JSON.parse(fs.readFileSync(path.join(repo, "ci", "gate.config.json"), "utf8")).rust ?? {};
  let solo = []; try { solo = JSON.parse(fs.readFileSync(path.join(repo, "ci", "shards.json"), "utf8")).solo ?? []; } catch { solo = []; }
  const wanted = units ? new Set(units) : null;
  const entries = [];
  for (const p of members) {
    if (wanted && !wanted.has(p.name)) continue;
    const { lintSets } = rustCratePlan(config[p.name] ?? {});
    for (const set of lintSets) entries.push({ name: p.name, crate: p.name, ws: ".", set, count: lintSets.length, env: null });
  }
  const sidecarBound = new Set(entries.filter((e) => sidecarBoundCrate({ dir: path.dirname(byId.get(members.find((m) => m.name === e.name).id).manifest_path) }).bound).map((e) => e.name));
  const toolchainNow = () => toolchainIdentity({ spawn: (c, a, o) => spawnSync(c, a, { ...o, encoding: "utf8" }), env: runEnv, cwd: repo });
  const toolchain = toolchainNow();
  log(`equivalence: ${entries.length} crate configuration(s) of ${members.length} workspace member(s); toolchain ${toolchain}`);

  const perCrateRuns = new Map();
  const perCrate = (entry) => {
    const key = `${entry.name}\0${entry.set.join(" ")}`;
    if (!perCrateRuns.has(key)) {
      const args = withKeepGoing(clippyArgs(entry.crate, { features: entry.set, messageFormat: "json", locked: true }));
      const r = { toolchain: toolchainNow(), ...cargoRun({ cargo, args, cwd: repo, env: runEnv, clean: [entry.crate], messagesFile: path.join(out, `percrate-${sha256(key).slice(0, 10)}.jsonl`) }) };
      log(`per-crate ${entry.name} [${entry.set.join(" ") || "default"}]: exit ${r.status} ${r.ms} ms ${r.bytes} bytes`);
      perCrateRuns.set(key, { ...r, argv: args });
    }
    return perCrateRuns.get(key);
  };
  const runPass = (separate) => {
    const plan = clippyBatchPlan(entries, { solo, separate, sidecarBound, toolchain: cargo });
    const groups = [];
    for (const b of plan.batches) {
      const args = withKeepGoing(clippyArgs(b.crates, { features: b.set, messageFormat: "json", locked: true }));
      const r = { toolchain: toolchainNow(), ...cargoRun({ cargo, args, cwd: repo, env: runEnv, clean: b.crates, messagesFile: path.join(out, `batch-${sha256(b.key).slice(0, 10)}.jsonl`) }) };
      log(`batch ${b.id} (${b.crates.length} crate(s), ${b.set.join(" ") || "default"}): exit ${r.status} ${r.ms} ms ${r.bytes} bytes`);
      const own = entries.filter((e) => b.units.includes(e.name) && e.set.join(" ") === b.set.join(" "));
      groups.push({ id: b.id, units: b.units, reason: b.reason, argv: args, exit: r.status, batch: { messages: r.messages, toolchain: r.toolchain, exit: r.status }, perCrate: own.map((e) => { const p = perCrate(e); return { unit: e.name, crate: e.crate, messages: p.messages, toolchain: p.toolchain, exit: p.status, argv: p.argv }; }) });
    }
    return { plan, groups, result: compareEquivalence({ groups }) };
  };
  const strip = (pass) => ({ batches: pass.plan.batches.map((b) => ({ id: b.id, crates: b.crates, set: b.set, reason: b.reason, order: b.order })), notes: pass.plan.notes, groups: pass.result.groups, failures: pass.result.failures, exits: pass.groups.map((g) => ({ id: g.id, batchExit: g.exit, perCrateExits: g.perCrate.map((p) => [p.unit, p.exit]) })) });
  const first = runPass(new Map());
  let final = first; let second = null;
  if (first.result.separate.size) { second = runPass(first.result.separate); final = second; }
  const report = {
    schema: EQUIVALENCE_SCHEMA, at: new Date().toISOString(), repo, toolchain, cargo,
    coverage: { workspaceMembers: members.length, crateConfigurations: entries.length, units: [...new Set(entries.map((e) => e.name))].sort(), sidecarBound: [...sidecarBound], perCrateRuns: perCrateRuns.size, firstPassBatches: first.plan.batches.length },
    equivalent: final.result.failures.filter((f) => !/are kept separate/.test(f)).length === 0 && final.result.separate.size === 0,
    firstPass: strip(first), firstPassSeparate: Object.fromEntries(first.result.separate),
    afterSplit: second ? strip(second) : null,
  };
  fs.writeFileSync(path.join(out, "equivalence.json"), `${JSON.stringify(report, null, 2)}\n`);
  return report;
}

const invokedDirectly = (() => { try { return fs.realpathSync(process.argv[1]) === fs.realpathSync(fileURLToPath(import.meta.url)); } catch { return false; } })();
if (invokedDirectly) {
  const args = process.argv.slice(2);
  const opt = (name) => { const i = args.indexOf(name); return i < 0 ? null : args[i + 1]; };
  if (!opt("--repo") || !opt("--out")) { process.stderr.write("usage: clippy-equivalence.mjs --repo <dir> --out <dir> [--units a,b] [--target-dir <dir>] [--cargo cargo]\n"); process.exit(2); }
  const report = run({ repo: path.resolve(opt("--repo")), out: path.resolve(opt("--out")), units: opt("--units") ? opt("--units").split(",") : null, ...(opt("--target-dir") ? { targetDir: path.resolve(opt("--target-dir")) } : {}), ...(opt("--cargo") ? { cargo: opt("--cargo") } : {}) });
  process.stdout.write(`${JSON.stringify({ equivalent: report.equivalent, coverage: report.coverage, firstPassFailures: report.firstPass.failures, afterSplitFailures: report.afterSplit?.failures ?? null })}\n`);
  process.exit(report.equivalent ? 0 : 1);
}
