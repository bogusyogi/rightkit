// The compile stage for a set of crates, built together (design sections 6 to 11 and 17). Glue only: it reads the crates from the
// snapshot, plans the calls (calls.mjs), runs them through the broker seam (run-calls.mjs), runs the test programs (tests.mjs) and
// returns one result per crate. The caller holds the snapshot lock and has already written the Class A verdict.
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { osOfTriple, planCalls, planNotes } from "./calls.mjs";
import { fileStore, resultsByCrate, runCalls } from "./run-calls.mjs";
import { runTests } from "./tests.mjs";
import { CROSS_TARGETS, hostTarget } from "../cross-targets.mjs";
import { runSeamCargoCall } from "../seam-broker.mjs";

export const UNIT_RESULT_SCHEMA = "rightkit.unit-result/1";
const HERE = path.dirname(fileURLToPath(import.meta.url));

/** What the call planner needs to know about one crate, read from its manifest in the snapshot and from ci/gate.config.json. */
export function crateInfo(snapshot, name, rustConfig = {}, { readFile = (f) => fs.readFileSync(f, "utf8"), exists = fs.existsSync } = {}) {
  const dir = path.join(snapshot, "crates", name);
  const manifest = readFile(path.join(dir, "Cargo.toml"));
  const features = /^\[features\]\s*$([\s\S]*?)(?=^\[|(?![\s\S]))/m.exec(manifest)?.[1] ?? "";
  const declaresFeatures = features.split(/\r?\n/).some((l) => /^\s*[A-Za-z0-9_-]+\s*=/.test(l) && !/^\s*default\s*=/.test(l));
  const cfg = rustConfig[name];
  return { name, dir, hasLib: exists(path.join(dir, "src", "lib.rs")) || /^\[lib\]/m.test(manifest), declaresFeatures, platforms: cfg?.platforms, config: cfg, quiet: cfg?.quietTests === true, serialToo: cfg?.alsoSerial === true, allowIgnored: Boolean(cfg?.allowIgnored) };
}

/** The identity a passed call is remembered under: the commit, the toolchain the snapshot pins, the host, the installed broker and this gate code. */
export function gateIdentity({ commit, snapshot, platform, client, readFile = (f) => fs.readFileSync(f, "utf8") }) {
  let toolchain = "unpinned";
  for (const file of ["rust-toolchain.toml", "rust-toolchain"]) { try { toolchain = crypto.createHash("sha256").update(readFile(path.join(snapshot, file))).digest("hex").slice(0, 16); break; } catch { /* next */ } }
  let broker = "none";
  if (client) { try { const m = JSON.parse(readFile(path.join(path.dirname(client), "runtime-manifest.json"))); broker = String(m.version ?? m.sourceRevision ?? "unknown"); } catch { broker = "unknown"; } }
  const code = crypto.createHash("sha256");
  for (const f of fs.readdirSync(HERE).filter((n) => n.endsWith(".mjs") && !n.endsWith(".test.mjs")).sort()) code.update(f).update(readFile(path.join(HERE, f)));
  return { commit, toolchain, host: platform, broker, gate: code.digest("hex").slice(0, 16) };
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
export async function compileTogether({ snapshot, commit, crates, rustConfig = {}, platform = process.platform, arch = process.arch, client = "rustright", env = process.env, verdictFile, storeDir, out = () => {}, onUnsettled = () => {}, send = null, runTestsWith = runTests }) {
  const infos = crates.map((name) => crateInfo(snapshot, name, rustConfig));
  const { hostTriple, otherTriples } = targetsFor(platform, arch);
  const calls = planCalls({ crates: infos, host: platform, hostTriple, otherTriples });
  const notes = planNotes({ crates: infos, host: platform });
  const identity = gateIdentity({ commit, snapshot, platform, client });
  const store = storeDir ? fileStore(storeDir) : null;
  const seamEnv = { ...env, RUSTRIGHT_GATE_COMMIT: commit, RUSTRIGHT_CLASS_A: verdictFile };
  const sendCall = send ?? ((call) => {
    const { result, ledger } = runSeamCargoCall(call.args, { cwd: path.join(snapshot, call.cwd ?? "."), env: seamEnv, client });
    if (ledger?.settled === false) onUnsettled(call, ledger);
    return result;
  });
  out(`together compile: ${infos.length} crate(s), ${calls.length} call(s) planned (${calls.filter((c) => c.crates.length > 1).length} name more than one crate; the largest names ${Math.max(0, ...calls.map((c) => c.crates.length))})`);
  const started = Date.now();
  const { outcomes, counts } = runCalls({
    calls, send: sendCall, store, identity, hostTriple,
    onCall: (o) => {
      const red = o.results.filter((r) => r.result !== "passed");
      out(`${o.state === "skipped" ? "skipped" : red.length ? "red    " : "passed "} ${o.call.id} (${o.call.crates.length} crate(s)${o.jobs.length > 1 ? ", green rerun sent" : ""})${red.length ? `: ${red.slice(0, 8).map((r) => `${r.crate} ${r.result}`).join(", ")}${red.length > 8 ? `, +${red.length - 8} more` : ""}` : ""}`);
    },
  });
  const perStage = resultsByCrate(outcomes);

  // Test programs: from the receipt of each test-build call's evidence job, for the crates that passed that call.
  const tests = {}; const testCounts = { programs: 0, ran: 0, cached: 0 };
  for (const o of outcomes.filter((x) => x.call.stage === "test-build")) {
    const green = o.results.filter((r) => r.result === "passed").map((r) => infos.find((i) => i.name === r.crate));
    if (!green.length) continue;
    let executables = null;
    try { executables = JSON.parse(fs.readFileSync(o.evidence.receipt, "utf8")).testExecutables ?? null; } catch { executables = null; }
    const ran = await runTestsWith({ executables, crates: green, env, store });
    for (const [name, r] of Object.entries(ran.byCrate)) {
      const cur = tests[name];
      if (!cur || (cur.result === "passed" && r.result !== "passed")) tests[name] = { ...r, round: o.call.round };
      else if (cur.result === "passed") tests[name] = { ...cur, programs: cur.programs + r.programs, passed: cur.passed + r.passed, ignored: cur.ignored + r.ignored };
    }
    for (const k of Object.keys(testCounts)) testCounts[k] += ran.counts[k];
  }

  const byCrate = {};
  for (const info of infos) {
    const stages = {};
    for (const s of STAGES) if (perStage[info.name]?.[s]) stages[s] = perStage[info.name][s];
    if (stages["test-build"]) stages.tests = stages["test-build"].result === "passed" ? (tests[info.name] ?? { result: "unknown", detail: "its tests were not run" }) : { result: "not-built", detail: "the test build did not pass" };
    const words = Object.values(stages).map((s) => s.result);
    const planned = words.length > 0;
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
