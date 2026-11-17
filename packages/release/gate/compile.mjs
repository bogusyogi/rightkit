// The compile stage for a set of crates, built together (design sections 6 to 11 and 17). Glue only: it reads the crates from the
// snapshot, plans the calls (calls.mjs), runs them through the broker seam (run-calls.mjs), runs the test programs (tests.mjs) and
// returns one result per crate. The caller holds the snapshot lock and has already written the Class A verdict.
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { DOC_DENY, osOfTriple, planCalls, planNotes } from "./calls.mjs";
import { fileStore, resultsByCrate, runCalls } from "./run-calls.mjs";
import { runTests } from "./tests.mjs";
import { CROSS_TARGETS, hostTarget } from "../cross-targets.mjs";
import { runSeamCargoCall } from "../seam-broker.mjs";
import { codeClosure } from "../code-closure.mjs";

export const UNIT_RESULT_SCHEMA = "rightkit.unit-result/1";
const HERE = path.dirname(fileURLToPath(import.meta.url));

/** What the call planner needs to know about one crate, read from its manifest in the snapshot and from ci/gate.config.json. */
export function crateInfo(snapshot, name, rustConfig = {}, { readFile = (f) => fs.readFileSync(f, "utf8"), exists = fs.existsSync } = {}) {
  const dir = path.join(snapshot, "crates", name);
  const manifest = readFile(path.join(dir, "Cargo.toml"));
  const features = /^\[features\]\s*$([\s\S]*?)(?=^\[|(?![\s\S]))/m.exec(manifest)?.[1] ?? "";
  const declaresFeatures = features.split(/\r?\n/).some((l) => /^\s*[A-Za-z0-9_-]+\s*=/.test(l) && !/^\s*default\s*=/.test(l));
  const cfg = rustConfig[name];
  return { name, dir, hasLib: exists(path.join(dir, "src", "lib.rs")) || /^\[lib\]/m.test(manifest), declaresFeatures, platforms: cfg?.platforms, config: cfg, quiet: cfg?.quietTests === true, serialToo: cfg?.alsoSerial === true, // Named only by a written reason, as on the per-crate rows: `true` or an empty string names nothing.
    allowIgnored: typeof cfg?.allowIgnored === "string" && cfg.allowIgnored.trim() !== "" };
}

/**
 * Every file of this package the build-together path loads: the files of `dir` (tests excluded) and every file they import by a
 * relative path, followed to the end. A change to any of them (the broker seam, the test environment) must void remembered passes.
 * An import that cannot be read throws: a pass is never remembered under a hash that skipped a file.
 */
export function gateCodeFiles(dir = HERE, { readFile = (f) => fs.readFileSync(f, "utf8"), list = (d) => fs.readdirSync(d) } = {}) {
  return codeClosure(list(dir).filter((n) => n.endsWith(".mjs") && !n.endsWith(".test.mjs")).map((n) => path.join(dir, n)), dir, { readFile });
}

/**
 * The identity a passed call is remembered under: the commit, the toolchain the snapshot pins, the host, the installed broker and this gate code.
 * The broker is named by its exact build (`brokerDigest` of the runtime manifest beside the client), never by a version label, which two builds
 * can share. `brokerKnown` is false when that digest cannot be read; then nothing is remembered and nothing remembered is reused.
 */
export function gateIdentity({ commit, snapshot, platform, client, readFile = (f) => fs.readFileSync(f, "utf8") }) {
  let toolchain = "unpinned";
  for (const file of ["rust-toolchain.toml", "rust-toolchain"]) { try { toolchain = crypto.createHash("sha256").update(readFile(path.join(snapshot, file))).digest("hex").slice(0, 16); break; } catch { /* next */ } }
  let broker = "none";
  if (client) { try { const m = JSON.parse(readFile(path.join(path.dirname(client), "runtime-manifest.json"))); broker = typeof m.brokerDigest === "string" && /^[0-9a-f]{64}$/.test(m.brokerDigest) ? m.brokerDigest : "unknown"; } catch { broker = "unknown"; } }
  const code = crypto.createHash("sha256");
  for (const f of gateCodeFiles(HERE)) code.update(f.name).update("\0").update(f.text).update("\0");
  return { commit, toolchain, host: platform, broker, brokerKnown: /^[0-9a-f]{64}$/.test(broker), gate: code.digest("hex").slice(0, 16) };
}

/**
 * The environment one call is sent with. A docs call denies rustdoc warnings, as CI does: the broker adds nothing itself and admits exactly this
 * value on doc calls. Every other call is sent without RUSTDOCFLAGS, whatever the launching session had set.
 */
export function callEnv(env, call) {
  const out = { ...env }; delete out.RUSTDOCFLAGS;
  if (call.stage === "docs") out.RUSTDOCFLAGS = DOC_DENY;
  return out;
}

/** The targets linted on this host: its own triple and the other operating systems' cross targets. */
export function targetsFor(platform = process.platform, arch = process.arch) {
  const hostTriple = hostTarget(platform, arch);
  return { hostTriple, otherTriples: Object.entries(CROSS_TARGETS).filter(([os]) => os !== platform).map(([, triple]) => triple).filter((t) => osOfTriple(t) !== platform) };
}

const STAGES = ["lint", "test-build", "docs"];

/**
 * Compiles `crates` (names) of the snapshot together and runs their tests.
 * `send`: optional replacement for the broker call (tests). `onUnsettled`: called for a broker job that ended without a settled outcome (the caller keeps the snapshot lock).
 * Returns { code, byCrate: { name: { result, stages: { lint, "test-build", docs, tests } } }, counts, notes, calls }.
 */
export async function compileTogether({ snapshot, commit, crates, rustConfig = {}, platform = process.platform, arch = process.arch, client = "rustright", env = process.env, verdictFile, storeDir, out = () => {}, onUnsettled = () => {}, send = null, runTestsWith = runTests, identityOf = gateIdentity }) {
  // ci/asset-gated.json of the commit: a listed crate runs its tests with RIGHTKIT_REQUIRE_ASSETS=0, every other with 1, as on the per-crate rows.
  let gated = {};
  try { gated = JSON.parse(fs.readFileSync(path.join(snapshot, "ci", "asset-gated.json"), "utf8")).crates ?? {}; } catch { gated = {}; }
  const infos = crates.map((name) => ({ ...crateInfo(snapshot, name, rustConfig), assetGated: Object.hasOwn(gated, name) }));
  const { hostTriple, otherTriples } = targetsFor(platform, arch);
  const calls = planCalls({ crates: infos, host: platform, hostTriple, otherTriples });
  const notes = planNotes({ crates: infos, host: platform });
  const identity = identityOf({ commit, snapshot, platform, client });
  // Without the broker's exact build there is nothing sound to remember a pass under, and no remembered pass can be shown to be this broker's.
  const store = storeDir && identity.brokerKnown ? fileStore(storeDir) : null;
  if (storeDir && !identity.brokerKnown) out("nothing is remembered or reused in this run: the installed broker's build (brokerDigest of its runtime manifest) cannot be read");
  // What a remembered test pass belongs to: this commit on this toolchain, host, broker and gate code, the same identity a remembered call has.
  const testScope = crypto.createHash("sha256").update(JSON.stringify(["commit", "toolchain", "host", "broker", "gate"].map((k) => identity[k]))).digest("hex");
  const seamEnv = { ...env, RUSTRIGHT_GATE_COMMIT: commit, RUSTRIGHT_CLASS_A: verdictFile };
  const sendCall = send ?? ((call) => {
    const { result, ledger } = runSeamCargoCall(call.args, { cwd: path.join(snapshot, call.cwd ?? "."), env: callEnv(seamEnv, call), client });
    if (ledger?.settled === false) onUnsettled(call, ledger);
    return result;
  });
  out(`together compile: ${infos.length} crate(s), ${calls.length} call(s) planned (${calls.filter((c) => c.crates.length > 1).length} name more than one crate; the largest names ${Math.max(0, ...calls.map((c) => c.crates.length))})`);
  const started = Date.now();
  const tests = {}; const testCounts = { programs: 0, ran: 0, cached: 0, unreaped: 0 };
  // Test programs: from the receipt of a test-build call's evidence job, for the crates that passed that call. They run as soon as that call
  // ends and before the next call is sent: a test that starts its crate's own binary (CARGO_BIN_EXE_<name>) reads it from the shared target
  // directory, where the next test build of the same crate with other features replaces it (Dell, 2026-11-11: rightkit-agent-tools'
  // default-build test ran against the binary of the later `demo` build and failed).
  const runTestsOf = async (o) => {
    const green = o.results.filter((r) => r.result === "passed").map((r) => infos.find((i) => i.name === r.crate));
    if (!green.length) return;
    let executables = null;
    try { executables = JSON.parse(fs.readFileSync(o.evidence.receipt, "utf8")).testExecutables ?? null; } catch { executables = null; }
    const ran = await runTestsWith({ executables, crates: green, env, store, scope: testScope });
    for (const [name, r] of Object.entries(ran.byCrate)) {
      const cur = tests[name];
      if (!cur || (cur.result === "passed" && r.result !== "passed")) tests[name] = { ...r, round: o.call.round };
      else if (cur.result === "passed") tests[name] = { ...cur, programs: cur.programs + r.programs, passed: cur.passed + r.passed, ignored: cur.ignored + r.ignored };
    }
    for (const k of Object.keys(testCounts)) testCounts[k] += ran.counts[k] ?? 0;
    // A test program that could not be proven stopped may still write in the snapshot: the caller keeps the snapshot lock, as for an unsettled broker job.
    if (ran.counts.unreaped) onUnsettled(o.call, { settled: false, reason: `${ran.counts.unreaped} test program(s) timed out and are not proven stopped` });
  };
  const outcomes = []; const counts = { calls: calls.length, ran: 0, skipped: 0, jobs: 0, greenReruns: 0, notRemembered: 0 };
  let stopped = null;
  for (const call of calls) {
    const one = runCalls({
      calls: [call], send: sendCall, store, identity, hostTriple, env: (c) => callEnv(seamEnv, c),
      onCall: (o) => {
        if (o.notRemembered) out(`not remembered ${o.call.id}: ${o.notRemembered}`);
        const red = o.results.filter((r) => r.result !== "passed");
        out(`${o.state === "skipped" ? "skipped" : red.length ? "red    " : "passed "} ${o.call.id} (${o.call.crates.length} crate(s)${o.jobs.length > 1 ? ", green rerun sent" : ""})${red.length ? `: ${red.slice(0, 8).map((r) => `${r.crate} ${r.result}`).join(", ")}${red.length > 8 ? `, +${red.length - 8} more` : ""}` : ""}`);
      },
    });
    outcomes.push(...one.outcomes);
    for (const k of ["ran", "skipped", "jobs", "greenReruns", "notRemembered"]) counts[k] += one.counts[k];
    if (call.stage === "test-build") for (const o of one.outcomes) await runTestsOf(o);
    // A test program not proven stopped may still be reading or writing the shared build folder: no further Cargo call is sent into it.
    if (testCounts.unreaped) { stopped = `stopped after ${call.id}: ${testCounts.unreaped} test program(s) timed out and are not proven stopped; ${calls.length - calls.indexOf(call) - 1} call(s) not sent`; out(stopped); break; }
  }
  const perStage = resultsByCrate(outcomes);

  const byCrate = {};
  for (const info of infos) {
    const stages = {};
    for (const s of STAGES) if (perStage[info.name]?.[s]) stages[s] = perStage[info.name][s];
    if (stages["test-build"]) stages.tests = stages["test-build"].result === "passed" ? (tests[info.name] ?? { result: "unknown", detail: "its tests were not run" }) : { result: "not-built", detail: "the test build did not pass" };
    const words = Object.values(stages).map((s) => s.result);
    const planned = words.length > 0;
    // A stopped run did not send every call: a crate with nothing red is unknown, never passed.
    if (stopped && planned && !words.includes("failed")) { byCrate[info.name] = { result: "unknown", detail: stopped, stages }; continue; }
    byCrate[info.name] = { result: !planned ? (notes.notOnThisHost.includes(info.name) ? "not-on-this-host" : "unknown") : words.every((w) => w === "passed") ? "passed" : words.includes("failed") ? "failed" : words.includes("not-built") ? "not-built" : "unknown", stages };
  }
  const allCounts = { ...counts, tests: testCounts, seconds: Math.round((Date.now() - started) / 1000) };
  out(`together compile: calls ${counts.ran} ran, ${counts.skipped} skipped, ${counts.jobs} broker job(s), ${counts.greenReruns} green rerun(s); test programs ${testCounts.ran} ran, ${testCounts.cached} cached; ${allCounts.seconds} s`);
  const owed = Object.entries(byCrate).filter(([, v]) => v.result !== "not-on-this-host");
  const code = owed.length > 0 && owed.every(([, v]) => v.result === "passed") ? 0 : 1;
  return { code, byCrate, counts: allCounts, notes, calls: outcomes.map((o) => ({ id: o.call.id, stage: o.call.stage, cwd: o.call.cwd ?? ".", args: o.call.args, state: o.state, jobs: o.jobs, evidence: o.evidence, crates: o.results })) };
}

/** The result file of one unit: what a publisher or CI admission reads. A unit that passed has one even when another unit of the run failed. */
export function unitResult({ selector, commit, tree, platform, crate, run }) {
  const mine = run.byCrate[crate];
  return {
    schema: UNIT_RESULT_SCHEMA, unit: selector, commit, tree, host: platform, stage: "compile",
    result: mine?.result ?? "unknown",
    stages: mine?.stages ?? {},
    calls: run.calls.filter((c) => c.crates.some((r) => r.crate === crate)).map((c) => ({ id: c.id, stage: c.stage, cwd: c.cwd, args: c.args, state: c.state, result: c.crates.find((r) => r.crate === crate).result, evidence: c.evidence })),
  };
}
