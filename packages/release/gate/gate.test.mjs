// Unit tests of the fresh gate's pure parts. `node --test packages/release/gate/gate.test.mjs`. No broker, no cargo.
import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { combineResults, crateResults, packageNameOf, readCargoStream, tripleOfArtifact } from "./crate-results.mjs";
import { cratePlan, osOfTriple, planCalls, planNotes } from "./calls.mjs";

const WIN = "x86_64-pc-windows-msvc"; const LIN = "x86_64-unknown-linux-gnu"; const MAC = "aarch64-apple-darwin";
const line = (o) => JSON.stringify(o);
const artifact = (name, triple, kind = "lib") => line({ reason: "compiler-artifact", package_id: `path+file:///D:/ws/crates/${name}#0.1.0`, target: { kind: [kind], name }, filenames: [`D:\\t\\${triple}\\release\\deps\\lib${name}.rmeta`], fresh: false });
const error = (name, text = "mismatched types") => line({ reason: "compiler-message", package_id: `path+file:///D:/ws/crates/${name}#0.1.0`, target: { kind: ["lib"], name }, message: { level: "error", rendered: `error: ${text}\n` } });
const warning = (name) => line({ reason: "compiler-message", package_id: `path+file:///D:/ws/crates/${name}#0.1.0`, message: { level: "warning", rendered: "warning: unused\n" } });
const finished = (success) => line({ reason: "build-finished", success });

test("package ids of every Cargo spelling give the crate name", () => {
  assert.equal(packageNameOf("path+file:///D:/ws/crates/rightkit-fs#0.4.3"), "rightkit-fs");
  assert.equal(packageNameOf("path+file:///D:/ws/ci/consumers/wv2#wv2-consumer@0.1.0"), "wv2-consumer");
  assert.equal(packageNameOf("registry+https://github.com/rust-lang/crates.io-index#serde@1.0.229"), "serde");
  assert.equal(packageNameOf("rightkit-fs 0.4.3 (path+file:///D:/ws/crates/rightkit-fs)"), "rightkit-fs");
});

test("an artifact's triple comes from its path; no triple in the path means the host", () => {
  assert.equal(tripleOfArtifact([`D:\\t\\${LIN}\\release\\deps\\liba.rmeta`], [WIN, LIN], WIN), LIN);
  assert.equal(tripleOfArtifact(["/t/release/deps/liba.rmeta"], [MAC, LIN], MAC), MAC);
});

test("all crates built: every crate passes", () => {
  const stdout = [artifact("a", WIN), artifact("a", LIN), artifact("b", WIN), artifact("b", LIN), finished(true)].join("\n");
  const r = crateResults({ crates: [{ name: "a", targets: [WIN, LIN] }, { name: "b", targets: [WIN, LIN] }], stdout, stderr: "", exit: 0, host: WIN });
  assert.deepEqual(r.map((x) => x.result), ["passed", "passed"]);
});

test("one crate with an error fails alone; its dependent is not built; an unrelated crate still passes", () => {
  const stdout = [artifact("c", WIN), error("a"), finished(false)].join("\n");
  const r = crateResults({ crates: [{ name: "a", targets: [WIN] }, { name: "b", targets: [WIN] }, { name: "c", targets: [WIN] }], stdout, stderr: "error: could not compile `a` (lib) due to 1 previous error", exit: 101, host: WIN });
  assert.deepEqual(r.map((x) => [x.crate, x.result]), [["a", "failed"], ["b", "not-built"], ["c", "passed"]]);
  assert.match(r[1].detail, /failed in this call: a/);
});

test("a crate that built for one target and not another is not built, not passed", () => {
  const stdout = [artifact("a", WIN), finished(false)].join("\n");
  const r = crateResults({ crates: [{ name: "a", targets: [WIN, LIN] }], stdout, stderr: "", exit: 101, host: WIN });
  assert.equal(r[0].result, "not-built");
  assert.match(r[0].detail, new RegExp(LIN));
});

test("a build-script failure is plain stderr text and still names its crate", () => {
  const stdout = [artifact("b", WIN), finished(false)].join("\n");
  const stderr = "error: failed to run custom build command for `rightkit-tts-qwen-sys v0.1.5 (D:\\ws\\crates\\rightkit-tts-qwen-sys)`";
  const r = crateResults({ crates: [{ name: "rightkit-tts-qwen-sys", targets: [WIN] }, { name: "b", targets: [WIN] }], stdout, stderr, exit: 101, host: WIN });
  assert.deepEqual(r.map((x) => x.result), ["failed", "passed"]);
});

test("warnings do not fail a crate; custom-build artifacts do not count as the crate's artifact", () => {
  const stdout = [warning("a"), artifact("a", WIN, "custom-build"), finished(false)].join("\n");
  const r = crateResults({ crates: [{ name: "a", targets: [WIN] }], stdout, stderr: "", exit: 101, host: WIN });
  assert.equal(r[0].result, "not-built");
});

test("a stream cut short passes nothing; a refusal passes nothing", () => {
  const cut = crateResults({ crates: [{ name: "a", targets: [WIN] }], stdout: artifact("a", WIN), stderr: "", exit: null, host: WIN });
  assert.equal(cut[0].result, "unknown");
  const none = crateResults({ crates: [{ name: "a", targets: [WIN] }], stdout: "", stderr: "", exit: 125, host: WIN });
  assert.equal(none[0].result, "unknown");
  const refused = crateResults({ crates: [{ name: "a", targets: [WIN] }], refusal: "RK_APP_BUILD_IN_PROGRESS: another build holds the app" });
  assert.deepEqual(refused, [{ crate: "a", result: "unknown", detail: "RK_APP_BUILD_IN_PROGRESS: another build holds the app" }]);
});

test("a crate passes a stage only when it passed every call that named it", () => {
  assert.equal(combineResults([{ result: "passed" }, { result: "passed" }]).result, "passed");
  assert.equal(combineResults([{ result: "passed" }, { result: "not-built", detail: "x" }]).result, "not-built");
  assert.equal(combineResults([{ result: "unknown", detail: "u" }, { result: "failed", detail: "f" }]).result, "failed");
  assert.equal(combineResults([]).result, "unknown");
});

test("malformed JSON lines are counted, not fatal", () => {
  const s = readCargoStream({ stdout: `{broken\n${finished(true)}`, targets: [WIN], host: WIN });
  assert.equal(s.malformed, 1); assert.equal(s.finished, true);
});

// ---- the call plan -------------------------------------------------------------------------------------------

const crate = (name, extra = {}) => ({ name, hasLib: true, declaresFeatures: false, ...extra });
const plan = (crates, host = "win32") => planCalls({ crates, host, hostTriple: host === "win32" ? WIN : host === "darwin" ? MAC : LIN, otherTriples: [WIN, LIN, MAC].filter((t) => osOfTriple(t) !== host) });

test("plain crates: five together calls, every crate in each; lint is one call per target, never two targets in one call", () => {
  const calls = plan([crate("a"), crate("b"), crate("c")]);
  assert.deepEqual(calls.map((c) => c.id), ["lint:default:win32", "lint:default:linux", "lint:default:darwin", "test-build:default", "docs:default"]);
  for (const c of calls) assert.deepEqual(c.crates.map((x) => x.name), ["a", "b", "c"]);
  for (const c of calls) assert.equal(c.args.filter((a) => a === "--target").length, 1, `${c.id} names exactly one target`);
  assert.deepEqual(calls.slice(0, 3).map((c) => c.args[c.args.indexOf("--target") + 1]), [WIN, LIN, MAC]);
  assert.deepEqual(calls[0].args.slice(-3), ["--", "-D", "warnings"]);
  assert.ok(calls[0].args.includes("--message-format=json") && calls[4].args.includes("--message-format=json"));
});

test("216 plain crates are still five calls: the call count does not grow with the crate count", () => {
  const many = Array.from({ length: 216 }, (_, i) => crate(`c${String(i).padStart(3, "0")}`));
  assert.equal(plan(many).length, 5);
});

test("a crate limited to some platforms is left out of the targets it cannot build and off hosts it does not run on", () => {
  const limited = crate("sys", { platforms: ["darwin", "win32"] });
  const calls = plan([crate("a"), limited]);
  const lint = calls.filter((c) => c.stage === "lint");
  assert.deepEqual(lint.map((c) => [c.crates.map((x) => x.name), c.crates[0].targets]), [[["a", "sys"], [WIN]], [["a"], [LIN]], [["a", "sys"], [MAC]]]);
  assert.equal(plan([limited], "linux").length, 0);
  assert.deepEqual(planNotes({ crates: [limited], host: "linux" }).notOnThisHost, ["sys"]);
});

test("a crate with features and no config gets default and --all-features, both together with its peers", () => {
  const calls = plan([crate("a", { declaresFeatures: true }), crate("b", { declaresFeatures: true }), crate("c")]);
  const all = calls.find((c) => c.id === "lint:all-features:win32");
  assert.deepEqual(all.crates.map((x) => x.name), ["a", "b"]);
  assert.ok(all.args.includes("--all-features"));
  assert.ok(calls.filter((c) => c.round === "all-features").every((c) => c.stage === "lint"), "all-features is lint only");
});

test("declared feature sets: each is its own single-crate call set; a host-scoped set runs only there and gets no other-OS lint", () => {
  const db = crate("db", { declaresFeatures: true, config: { featureSets: [["--features", "bundled"], ["--features", "sqlcipher-libtomcrypt"]], checkFeatureSets: [{ features: ["--features", "sqlcipher-system"], hosts: ["darwin", "linux"], hostsReason: "needs system crypto" }] } });
  const win = plan([db]);
  assert.deepEqual(win.map((c) => c.id), ["lint:db[bundled]:win32", "lint:db[bundled]:linux", "lint:db[bundled]:darwin", "test-build:db[bundled]", "docs:db[bundled]", "lint:db[sqlcipher-libtomcrypt]:win32", "lint:db[sqlcipher-libtomcrypt]:linux", "lint:db[sqlcipher-libtomcrypt]:darwin", "test-build:db[sqlcipher-libtomcrypt]", "docs:db[sqlcipher-libtomcrypt]"]);
  assert.ok(!win.some((c) => c.round === "default"), "a crate that declares sets without [] is not in the default round");
  const mac = plan([db], "darwin");
  const scoped = mac.filter((c) => c.id.startsWith("lint:db[sqlcipher-system]"));
  assert.deepEqual(scoped.map((c) => c.crates[0].targets), [[MAC]], "a host-scoped set is linted for its own OS only");
});

test("unlinted features: no --all-features call, and the reason is carried to the summary", () => {
  const qa = crate("qa", { declaresFeatures: true, config: { unlintedFeatures: { "wv2-0_38": "cannot type-check against Tauri 2.12 in this workspace" } } });
  assert.ok(!plan([qa]).some((c) => c.args.includes("--all-features")));
  assert.deepEqual(planNotes({ crates: [qa], host: "win32" }).notLinted, { qa: { "wv2-0_38": "cannot type-check against Tauri 2.12 in this workspace" } });
});

test("a crate with no library gets no docs call; consumer manifests are their own lint call in their own directory", () => {
  const bin = crate("tool", { hasLib: false, config: { consumerRows: [{ manifestPath: "ci/consumers/wv2-0_39/Cargo.toml", args: ["--features", "qa"], locked: true }] } });
  const calls = plan([bin]);
  assert.ok(!calls.some((c) => c.stage === "docs"));
  const consumer = calls.find((c) => c.id === "lint:consumer:ci/consumers/wv2-0_39");
  assert.equal(consumer.cwd, "ci/consumers/wv2-0_39");
});

test("the real ci/gate.config.json plans without error on every host, and the together calls carry almost every crate", () => {
  const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..", "..");
  const config = JSON.parse(fs.readFileSync(path.join(root, "ci", "gate.config.json"), "utf8")).rust;
  const names = fs.readdirSync(path.join(root, "crates")).filter((d) => fs.existsSync(path.join(root, "crates", d, "Cargo.toml")));
  const crates = names.map((name) => {
    const manifest = fs.readFileSync(path.join(root, "crates", name, "Cargo.toml"), "utf8");
    const features = /^\[features\]\s*$([\s\S]*?)(?=^\[|\Z)/m.exec(manifest)?.[1] ?? "";
    const declaresFeatures = features.split(/\r?\n/).some((l) => /^\s*[A-Za-z0-9_-]+\s*=/.test(l) && !/^\s*default\s*=/.test(l));
    return { name, hasLib: fs.existsSync(path.join(root, "crates", name, "src", "lib.rs")) || /^\[lib\]/m.test(manifest), declaresFeatures, platforms: config[name]?.platforms, config: config[name] };
  });
  for (const host of ["win32", "darwin", "linux"]) {
    const calls = plan(crates, host);
    const together = calls.filter((c) => c.round === "default" || c.round === "all-features");
    const single = calls.length - together.length;
    const biggest = Math.max(...together.map((c) => c.crates.length));
    assert.ok(together.length <= 16, `${host}: ${together.length} together calls`);
    assert.ok(biggest >= names.length * 0.8, `${host}: the largest together call names ${biggest} of ${names.length} crates`);
    assert.ok(calls.length < 500, `${host}: ${calls.length} calls`);
    console.log(`${host}: ${names.length} crates -> ${calls.length} calls (${together.length} together, largest names ${biggest} crates; ${single} single-crate feature or consumer calls)`);
  }
});

// ---- running the calls ---------------------------------------------------------------------------------------
import os from "node:os";
import { callKey, fileStore, restrictCall, resultsByCrate, runCalls } from "./run-calls.mjs";

const IDENT = { commit: "a".repeat(40), toolchain: "1.99.1", host: "win32", broker: "rustright-f448a440", gate: "0.7.0" };
/** A fake broker: `red` crates produce an error, crates depending on a red crate (`deps`) produce no artifact, every job is recorded. */
function fakeBroker({ red = [], deps = {} } = {}) {
  const sent = [];
  const send = (call) => {
    sent.push(call);
    const lines = []; let failed = false; const stderr = [];
    for (const c of call.crates) {
      if (red.includes(c.name)) { lines.push(error(c.name)); stderr.push(`error: could not compile \`${c.name}\` (lib) due to 1 previous error`); failed = true; continue; }
      if ((deps[c.name] ?? []).some((d) => red.includes(d))) { failed = true; continue; }
      for (const t of c.targets) lines.push(artifact(c.name, t));
    }
    lines.push(finished(!failed));
    return { status: failed ? 101 : 0, stdout: lines.join("\n"), stderr: stderr.join("\n"), requestId: `job-${sent.length}`, receipt: `/r/job-${sent.length}.json`, receiptSha256: "0".repeat(64) };
  };
  return { send, sent };
}
const tmpStore = () => fileStore(fs.mkdtempSync(path.join(os.tmpdir(), "gate-calls-")));

test("a clean run sends each call once; the same commit again sends nothing", () => {
  const calls = plan([crate("a"), crate("b"), crate("c")]);
  const store = tmpStore(); const broker = fakeBroker();
  const first = runCalls({ calls, send: broker.send, store, identity: IDENT, hostTriple: WIN });
  assert.deepEqual(first.counts, { calls: 5, ran: 5, skipped: 0, jobs: 5, greenReruns: 0 });
  const second = runCalls({ calls, send: broker.send, store, identity: IDENT, hostTriple: WIN });
  assert.deepEqual(second.counts, { calls: 5, ran: 0, skipped: 5, jobs: 0, greenReruns: 0 });
  assert.equal(broker.sent.length, 5, "the rerun reached the broker zero times");
  assert.ok(second.outcomes.every((o) => o.results.every((r) => r.result === "passed") && o.evidence?.requestId));
});

test("a new commit sends every call again: evidence is never reused across commits", () => {
  const calls = plan([crate("a")]); const store = tmpStore(); const broker = fakeBroker();
  runCalls({ calls, send: broker.send, store, identity: IDENT, hostTriple: WIN });
  const next = runCalls({ calls, send: broker.send, store, identity: { ...IDENT, commit: "b".repeat(40) }, hostTriple: WIN });
  assert.equal(next.counts.ran, 5);
  assert.notEqual(callKey(calls[0], IDENT), callKey(calls[0], { ...IDENT, gate: "0.7.1" }), "a gate change is a different key");
  assert.throws(() => callKey(calls[0], { ...IDENT, broker: "" }), /identity\.broker/);
});

test("one red crate fails alone: its dependent is not built, every other crate passes with signed evidence from a job that exited 0", () => {
  const calls = plan([crate("a"), crate("b"), crate("c"), crate("d")]).filter((c) => c.id === "lint:default:win32");
  const broker = fakeBroker({ red: ["a"], deps: { b: ["a"] } });
  const { outcomes, counts } = runCalls({ calls, send: broker.send, store: tmpStore(), identity: IDENT, hostTriple: WIN });
  assert.deepEqual(outcomes[0].results.map((r) => [r.crate, r.result]), [["a", "failed"], ["b", "not-built"], ["c", "passed"], ["d", "passed"]]);
  assert.deepEqual(counts, { calls: 1, ran: 1, skipped: 0, jobs: 2, greenReruns: 1 });
  assert.deepEqual(broker.sent[1].crates.map((c) => c.name), ["c", "d"], "the second job names only the crates that passed");
  assert.deepEqual(broker.sent[1].args.filter((a, i, all) => all[i - 1] === "-p"), ["c", "d"]);
  assert.equal(outcomes[0].evidence.requestId, "job-2"); assert.equal(outcomes[0].jobs[1].exit, 0);
  const by = resultsByCrate(outcomes);
  assert.equal(by.c.lint.result, "passed"); assert.equal(by.c.lint.evidence[0].requestId, "job-2");
  assert.equal(by.a.lint.result, "failed"); assert.equal(by.b.lint.result, "not-built"); assert.equal(by.a.lint.evidence.length, 0);
});

test("a red call is never remembered: the next run sends it again; the green rerun is remembered under its own key", () => {
  const calls = plan([crate("a"), crate("c")]).filter((c) => c.id === "lint:default:win32");
  const store = tmpStore();
  runCalls({ calls, send: fakeBroker({ red: ["a"] }).send, store, identity: IDENT, hostTriple: WIN });
  const fixed = fakeBroker();
  const after = runCalls({ calls, send: fixed.send, store, identity: IDENT, hostTriple: WIN });
  assert.equal(after.counts.ran, 1, "the call that had a red crate runs again");
  assert.ok(store.get(callKey(restrictCall(calls[0], ["c"]), IDENT)), "the green-only call was remembered");
});

test("a refused call and a job that exits non-zero with nothing attributed prove nothing", () => {
  const calls = plan([crate("a")]).filter((c) => c.id === "lint:default:win32");
  const refused = runCalls({ calls, send: () => ({ status: 125, stdout: "", stderr: "", refusal: { code: "RK_APP_BUILD_IN_PROGRESS", message: "busy" } }), store: tmpStore(), identity: IDENT, hostTriple: WIN });
  assert.equal(refused.outcomes[0].results[0].result, "unknown"); assert.equal(refused.outcomes[0].evidence, null);
  const odd = runCalls({ calls, send: (c) => ({ status: 101, stdout: [...c.crates.flatMap((x) => x.targets.map((t) => artifact(x.name, t))), finished(false)].join("\n"), stderr: "" }), store: tmpStore(), identity: IDENT, hostTriple: WIN });
  assert.equal(odd.outcomes[0].results[0].result, "unknown");
});

test("restricting a call keeps everything but the dropped crates, including what follows --", () => {
  const call = plan([crate("a"), crate("b"), crate("c")])[0];
  const r = restrictCall(call, ["b"]);
  assert.deepEqual(r.args.filter((a, i, all) => all[i - 1] === "-p"), ["b"]);
  assert.deepEqual(r.args.slice(-3), ["--", "-D", "warnings"]);
  assert.deepEqual(r.args.filter((a) => a !== "-p" && !["a", "b", "c"].includes(a)), call.args.filter((a) => a !== "-p" && !["a", "b", "c"].includes(a)));
});

// ---- the compile stage end to end (fake broker, real files) ---------------------------------------------------
import { compileTogether, crateInfo, targetsFor, unitResult, UNIT_RESULT_SCHEMA } from "./compile.mjs";
import { libtestSummary, runTests } from "./tests.mjs";

function fakeSnapshot(crates) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "gate-snap-"));
  for (const [name, { lib = true, features = "" } = {}] of Object.entries(crates)) {
    fs.mkdirSync(path.join(dir, "crates", name, "src"), { recursive: true });
    fs.writeFileSync(path.join(dir, "crates", name, "Cargo.toml"), `[package]\nname = "${name}"\nversion = "0.1.0"\n${features ? `\n[features]\n${features}\n` : ""}`);
    fs.writeFileSync(path.join(dir, "crates", name, "src", lib ? "lib.rs" : "main.rs"), "");
  }
  return dir;
}
/** A fake broker that also writes a receipt file listing one test program per crate for test-build calls. */
function fakeSeam({ red = [], deps = {} } = {}) {
  const base = fakeBroker({ red, deps }); const dir = fs.mkdtempSync(path.join(os.tmpdir(), "gate-rcpt-"));
  const send = (call) => {
    const r = base.send(call);
    const file = path.join(dir, `${r.requestId}.json`);
    const built = call.stage === "test-build" && r.status === 0 ? call.crates.map((c) => ({ path: path.join(dir, `${c.name}.exe`), sha256: "ab".repeat(32), package_id: `path+file:///s/crates/${c.name}#0.1.0`, target: c.name, kind: "lib" })) : null;
    fs.writeFileSync(file, JSON.stringify({ id: r.requestId, testExecutables: built }));
    return { ...r, receipt: file };
  };
  return { send, sent: base.sent };
}
const fakeTests = ({ failing = [] } = {}) => async ({ executables, crates }) => ({ byCrate: Object.fromEntries(crates.map((c) => [c.name, Array.isArray(executables) && executables.some((e) => e.target === c.name) ? (failing.includes(c.name) ? { result: "failed", detail: "1 test failed", programs: 1, passed: 0, failed: 1, ignored: 0 } : { result: "passed", detail: "", programs: 1, passed: 3, failed: 0, ignored: 0 }) : { result: "unknown", detail: "no program listed", programs: 0, passed: 0, failed: 0, ignored: 0 }])), counts: { programs: crates.length, ran: crates.length, cached: 0 } });
const compile = (snapshot, crates, seam, extra = {}) => compileTogether({ snapshot, commit: "c".repeat(40), crates, platform: "win32", arch: "x64", client: null, verdictFile: path.join(snapshot, "verdict.json"), storeDir: path.join(snapshot, ".calls"), send: seam.send, runTestsWith: fakeTests(extra.tests), ...extra });

test("crate facts are read from the snapshot: library, declared features, config", () => {
  const snap = fakeSnapshot({ a: {}, b: { lib: false, features: 'default = []\nextra = []' } });
  assert.deepEqual([crateInfo(snap, "a").hasLib, crateInfo(snap, "a").declaresFeatures], [true, false]);
  assert.deepEqual([crateInfo(snap, "b").hasLib, crateInfo(snap, "b").declaresFeatures], [false, true]);
  assert.equal(crateInfo(snap, "a", { a: { quietTests: true, alsoSerial: true } }).quiet, true);
  assert.deepEqual(targetsFor("win32", "x64"), { hostTriple: WIN, otherTriples: [LIN, MAC] });
});

test("compile stage, all green: exit 0, five broker jobs for any number of crates, every crate passed with tests; rerun sends nothing", async () => {
  const snap = fakeSnapshot({ a: {}, b: {}, c: {}, d: {} }); const seam = fakeSeam();
  const run = await compile(snap, ["a", "b", "c", "d"], seam);
  assert.equal(run.code, 0); assert.equal(run.counts.jobs, 5);
  assert.ok(Object.values(run.byCrate).every((v) => v.result === "passed" && v.stages.tests.result === "passed"));
  const again = await compile(snap, ["a", "b", "c", "d"], seam);
  assert.equal(again.code, 0); assert.equal(again.counts.jobs, 0); assert.equal(again.counts.skipped, 5);
  assert.equal(seam.sent.length, 5);
});

test("a test build's programs run before the next call is sent (a later build of the same crate replaces the binaries its tests start)", async () => {
  const snap = fakeSnapshot({ a: {} }); const seam = fakeSeam(); const order = [];
  const send = (call) => { order.push(call.id); return seam.send(call); };
  const tests = fakeTests();
  const run = await compileTogether({ snapshot: snap, commit: "c".repeat(40), crates: ["a"], platform: "win32", arch: "x64", client: null, verdictFile: path.join(snap, "verdict.json"), storeDir: path.join(snap, ".calls"), send, runTestsWith: async (input) => { order.push("tests"); return tests(input); } });
  assert.equal(run.code, 0);
  const at = order.indexOf("tests");
  assert.ok(at > 0 && order[at - 1].startsWith("test-build:"), order.join(" > "));
  assert.ok(at < order.length - 1, `a call follows the test build in this plan: ${order.join(" > ")}`);
});

test("compile stage, one red crate: exit 1, the red crate fails, its dependent is not built, the others pass with their own result file", async () => {
  const snap = fakeSnapshot({ a: {}, b: {}, c: {}, d: {} }); const seam = fakeSeam({ red: ["a"], deps: { b: ["a"] } });
  const run = await compile(snap, ["a", "b", "c", "d"], seam);
  assert.equal(run.code, 1);
  assert.deepEqual(Object.fromEntries(Object.entries(run.byCrate).map(([k, v]) => [k, v.result])), { a: "failed", b: "not-built", c: "passed", d: "passed" });
  const file = unitResult({ selector: "cargo:c", commit: "c".repeat(40), tree: "t", platform: "win32", crate: "c", run });
  assert.equal(file.schema, UNIT_RESULT_SCHEMA); assert.equal(file.result, "passed");
  assert.ok(file.calls.length === 5 && file.calls.every((c) => c.result === "passed" && c.evidence?.requestId), "each call that named the unit carries the job that proves it");
  assert.ok(file.calls.every((c) => c.id !== undefined && Array.isArray(c.args)));
  assert.equal(unitResult({ selector: "cargo:a", commit: "c".repeat(40), tree: "t", platform: "win32", crate: "a", run }).result, "failed");
});

test("compile stage: a failing test fails only its crate", async () => {
  const snap = fakeSnapshot({ a: {}, b: {} }); const run = await compile(snap, ["a", "b"], fakeSeam(), { tests: { failing: ["b"] } });
  assert.equal(run.code, 1);
  assert.equal(run.byCrate.a.result, "passed"); assert.equal(run.byCrate.b.result, "failed"); assert.equal(run.byCrate.b.stages.tests.result, "failed");
});

test("test programs: the receipt's sha256 must match the file; ignored-only crates fail unless allowed; results are per crate", async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "gate-tests-"));
  const exe = (name) => ({ path: path.join(dir, `${name}.exe`), sha256: "11".repeat(32), package_id: `path+file:///s/crates/${name}#0.1.0`, target: name });
  const outputs = { ok: "test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured\n", ign: "test result: ok. 0 passed; 0 failed; 2 ignored; 0 measured\n", bad: "test result: FAILED. 1 passed; 1 failed; 0 ignored\n" };
  const spawn = (file) => {
    const name = path.basename(file, ".exe"); const listeners = {};
    const child = { stdout: { on: (_, f) => f(outputs[name] ?? "") }, stderr: { on: () => {} }, on: (ev, f) => { listeners[ev] = f; if (ev === "close") setImmediate(() => f(name === "bad" ? 101 : 0, null)); }, kill: () => {} };
    return child;
  };
  const crates = ["ok", "ign", "bad", "tampered"].map((name) => ({ name, dir }));
  const r = await runTests({ executables: ["ok", "ign", "bad", "tampered"].map(exe), crates, spawn, hashOf: (f) => (f.includes("tampered") ? "22".repeat(32) : "11".repeat(32)) });
  assert.deepEqual(Object.fromEntries(Object.entries(r.byCrate).map(([k, v]) => [k, v.result])), { ok: "passed", ign: "failed", bad: "failed", tampered: "failed" });
  assert.match(r.byCrate.ign.detail, /no test passed/); assert.match(r.byCrate.tampered.detail, /sha256 differs/);
  const allowed = await runTests({ executables: [exe("ign")], crates: [{ name: "ign", dir, allowIgnored: true }], spawn, hashOf: () => "11".repeat(32) });
  assert.equal(allowed.byCrate.ign.result, "passed");
  const none = await runTests({ executables: null, crates: [{ name: "ok", dir }] });
  assert.equal(none.byCrate.ok.result, "unknown");
  assert.deepEqual(libtestSummary(outputs.ok + outputs.bad), { passed: 5, failed: 1, ignored: 0, lines: 2 });
});

// ---- the publisher reads the unit result ----------------------------------------------------------------------
import { compileReportProblems } from "../publish-local-cargo.mjs";

test("the publisher admits a passed unit result and refuses a red, misbound or evidence-less one", async () => {
  const snap = fakeSnapshot({ a: {}, c: {} }); const commit = "c".repeat(40);
  const run = await compile(snap, ["a", "c"], fakeSeam({ red: ["a"] }));
  const good = unitResult({ selector: "cargo:c", commit, tree: "t1", platform: "win32", crate: "c", run });
  assert.deepEqual(compileReportProblems(good, { name: "c", version: "0.1.0", commit, tree: "t1" }), []);
  assert.ok(compileReportProblems(unitResult({ selector: "cargo:a", commit, tree: "t1", platform: "win32", crate: "a", run }), { name: "a", version: "0.1.0", commit, tree: "t1" }).length > 0, "the red crate is refused");
  assert.match(compileReportProblems(good, { name: "c", version: "0.1.0", commit: "d".repeat(40), tree: "t1" }).join(";"), /commit/);
  assert.match(compileReportProblems(good, { name: "c", version: "0.1.0", commit, tree: "other" }).join(";"), /tree/);
  assert.match(compileReportProblems(good, { name: "x", version: "0.1.0", commit, tree: "t1" }).join(";"), /unit is/);
  const stripped = { ...good, calls: good.calls.map((c) => ({ ...c, evidence: null })) };
  assert.match(compileReportProblems(stripped, { name: "c", version: "0.1.0", commit, tree: "t1" }).join(";"), /no broker job/);
  const noTests = { ...good, stages: { ...good.stages, tests: { result: "unknown" } } };
  assert.match(compileReportProblems(noTests, { name: "c", version: "0.1.0", commit, tree: "t1" }).join(";"), /stage tests/);
});

// ---- consumer manifests ---------------------------------------------------------------------------------------
test("the same consumer manifest declared by three crates is one call; it is judged whole", () => {
  const row = { manifestPath: "ci/consumers/wv2-0_39/Cargo.toml", args: ["--features", "qa"], locked: true };
  const crates = ["qa", "qa-driver", "shell"].map((n) => crate(n, { config: { consumerRows: [row] } }));
  const calls = plan(crates).filter((c) => c.consumer);
  assert.equal(calls.length, 1); assert.deepEqual(calls[0].crates.map((c) => c.name), ["qa", "qa-driver", "shell"]);
  // The consumer's own package fails; the named crates built fine. The call still fails for all of them.
  const stdout = [artifact("qa", WIN), artifact("qa-driver", WIN), artifact("shell", WIN), error("wv2-0_39-consumer", "unused import"), finished(false)].join("\n");
  const red = runCalls({ calls, send: () => ({ status: 101, stdout, stderr: "", requestId: "j1", receiptSha256: "0".repeat(64) }), store: null, identity: IDENT, hostTriple: WIN });
  assert.deepEqual(red.outcomes[0].results.map((r) => r.result), ["failed", "failed", "failed"]);
  assert.match(red.outcomes[0].results[0].detail, /wv2-0_39-consumer/);
  const ok = runCalls({ calls, send: () => ({ status: 0, stdout: finished(true), stderr: "", requestId: "j2", receiptSha256: "0".repeat(64) }), store: null, identity: IDENT, hostTriple: WIN });
  assert.deepEqual(ok.outcomes[0].results.map((r) => r.result), ["passed", "passed", "passed"]);
  assert.equal(ok.outcomes[0].evidence.requestId, "j2");
});

test("a call the seam marks EXIT (Cargo ran and failed) is read as a build result: the red crate fails, the green ones get their rerun", () => {
  const calls = plan([crate("a"), crate("c"), crate("d")]).filter((c) => c.id === "lint:default:win32");
  const base = fakeBroker({ red: ["a"] });
  const send = (call) => { const r = base.send(call); return r.status === 0 ? r : { ...r, refusal: { code: "EXIT", message: `exit ${r.status}` } }; };
  const { outcomes, counts } = runCalls({ calls, send, store: tmpStore(), identity: IDENT, hostTriple: WIN });
  assert.deepEqual(outcomes[0].results.map((r) => [r.crate, r.result]), [["a", "failed"], ["c", "passed"], ["d", "passed"]]);
  assert.equal(counts.greenReruns, 1); assert.equal(outcomes[0].evidence.requestId, "job-2");
});

test("a test program never inherits a credential-named variable; assets are required unless the crate is asset-gated (loop review 11, D1)", async () => {
  const { EventEmitter } = await import("node:events");
  const { testProgramEnv, CREDENTIAL_NAME } = await import("../test-env.mjs");
  const seen = [];
  const spawn = (file, args, options) => {
    seen.push({ file, env: options.env });
    const child = new EventEmitter(); child.stdout = new EventEmitter(); child.stderr = new EventEmitter(); child.kill = () => {};
    setImmediate(() => { child.stdout.emit("data", "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n"); child.emit("close", 0, null); });
    return child;
  };
  const exe = (name) => ({ path: `/t/${name}`, sha256: "ab".repeat(32), package_id: `path+file:///s/crates/${name}#0.1.0`, target: name, kind: "lib" });
  const env = { PATH: "/bin", CARGO_REGISTRY_TOKEN: "x", AZURE_CLIENT_SECRET: "x", MY_PASSWORD: "x", OPENAI_API_KEY: "x", SIGNING_PRIVATE_KEY: "x", github_token: "x", HOME: "/h" };
  const ran = await runTests({ executables: [exe("a"), exe("b")], crates: [{ name: "a", dir: "/s/crates/a" }, { name: "b", dir: "/s/crates/b", assetGated: true }], env, jobs: 1, spawn, hashOf: () => "ab".repeat(32) });
  assert.equal(ran.byCrate.a.result, "passed"); assert.equal(ran.byCrate.b.result, "passed"); assert.equal(seen.length, 2);
  for (const s of seen) {
    for (const name of Object.keys(s.env)) assert.equal(CREDENTIAL_NAME.test(name), false, `${name} reached ${s.file}`);
    assert.equal(s.env.PATH, "/bin"); assert.equal(s.env.HOME, "/h"); assert.equal(s.env.RIGHTKIT_ISOLATED_CHECKOUT, "1"); assert.equal(s.env.CARGO_TERM_COLOR, "never");
  }
  assert.equal(seen.find((s) => s.file === "/t/a").env.RIGHTKIT_REQUIRE_ASSETS, "1");
  assert.equal(seen.find((s) => s.file === "/t/b").env.RIGHTKIT_REQUIRE_ASSETS, "0");
  assert.equal(seen.find((s) => s.file === "/t/a").env.CARGO_MANIFEST_DIR, "/s/crates/a");
  // The per-crate rows use the same function (local-publish.mjs), so the two paths cannot differ again.
  assert.deepEqual(Object.keys(testProgramEnv({ A_TOKEN: "1", B: "2" }, false)).sort(), ["B", "CARGO_TERM_COLOR", "RIGHTKIT_ISOLATED_CHECKOUT", "RIGHTKIT_REQUIRE_ASSETS"]);
  assert.match(fs.readFileSync(new URL("../local-publish.mjs", import.meta.url), "utf8"), /const testEnv = \(deps, isAssetGated\) => testProgramEnv\(/);
});

test("G19: the gate-code hash covers every file the build-together path imports, not only gate/", async () => {
  const { gateCodeFiles } = await import("./compile.mjs");
  const names = gateCodeFiles().map((f) => f.name);
  for (const want of ["compile.mjs", "tests.mjs", "run-calls.mjs", "../seam-broker.mjs", "../gate-seam.mjs", "../test-env.mjs", "../cross-targets.mjs"]) assert.ok(names.includes(want), `${want} is not in the hash`);
  assert.equal(names.some((n) => n.endsWith(".test.mjs")), false);
  // Followed to the end, each file once, in a fixed order; an unreadable import throws.
  const files = { "/g/a.mjs": 'import { b } from "../b.mjs";', "/b.mjs": 'export * from "./c.mjs"; const x = await import("./g/a.mjs");', "/c.mjs": "export const c = 1;" };
  const read = (f) => { const k = f.split(path.sep).join("/").replace(/^[A-Za-z]:/, ""); if (!(k in files)) throw new Error(`no ${k}`); return files[k]; };
  assert.deepEqual(gateCodeFiles(path.resolve("/g"), { readFile: read, list: () => ["a.mjs", "a.test.mjs", "notes.txt"] }).map((f) => f.name), ["../b.mjs", "../c.mjs", "a.mjs"]);
  files["/c.mjs"] = 'import "./gone.mjs";';
  assert.throws(() => gateCodeFiles(path.resolve("/g"), { readFile: read, list: () => ["a.mjs"] }), /no \/gone\.mjs/);
});

test("G17: a test program past its time limit is stopped as a tree; without proof the tree is gone it is unreaped and nothing more starts", async () => {
  const { EventEmitter } = await import("node:events");
  const { stopTree, treeGone } = await import("./tests.mjs");
  const exe = (name) => ({ path: `/t/${name}`, sha256: "ab".repeat(32), package_id: `path+file:///s/crates/${name}#0.1.0`, target: name, kind: "lib" });
  const fake = () => { const started = []; const options = [];
    const spawn = (file, args, opts) => { started.push(file); options.push(opts); const c = new EventEmitter(); c.pid = 4242; c.stdout = new EventEmitter(); c.stderr = new EventEmitter(); c.kill = () => { throw new Error("child.kill must not be the stop"); }; return c; };
    return { spawn, started, options };
  };
  const base = { executables: [exe("a"), exe("b")], crates: [{ name: "a", dir: "/s/crates/a" }, { name: "b", dir: "/s/crates/b" }], env: { PATH: "/bin" }, jobs: 1, hashOf: () => "ab".repeat(32), timeoutMs: 15, reapMs: 60, pollMs: 5 };
  const okStop = () => ({ ok: true, detail: "" });

  // POSIX, the group answers "no such process" after the stop: proven stopped, the next program still runs (it hangs too, and is proven too).
  const p = fake();
  const proven = await runTests({ ...base, spawn: p.spawn, stop: okStop, gone: () => true, platform: "linux" });
  assert.equal(p.options[0].detached, true); assert.deepEqual(p.started, ["/t/a", "/t/b"]);
  assert.equal(proven.byCrate.a.result, "failed"); assert.match(proven.byCrate.a.detail, /timed out after 0 s; the program and everything it started are proven stopped/); assert.equal(proven.counts.unreaped, 0);

  // POSIX, a surviving descendant: the program itself closed, the group still answers. Not proven: unreaped, and b is never started.
  const d = fake();
  const closing = (file, args, opts) => { const c = d.spawn(file, args, opts); setTimeout(() => c.emit("close", null, "SIGKILL"), 25); return c; };
  const survivor = await runTests({ ...base, spawn: closing, stop: okStop, gone: () => false, platform: "linux" });
  assert.equal(survivor.counts.unreaped, 1); assert.deepEqual(d.started, ["/t/a"]);
  assert.equal(survivor.byCrate.a.result, "failed"); assert.match(survivor.byCrate.a.detail, /not proven stopped \(its process group still answered/);
  assert.equal(survivor.byCrate.b.result, "unknown"); assert.match(survivor.byCrate.b.detail, /not run: an earlier test program is not proven stopped/);

  // Windows, the program ended after a clean taskkill: still no proof for what it started, so unreaped. The child is not detached there.
  const w = fake();
  const wClosing = (file, args, opts) => { const c = w.spawn(file, args, opts); setTimeout(() => c.emit("close", 1, null), 25); return c; };
  const win = await runTests({ ...base, spawn: wClosing, stop: okStop, platform: "win32" });
  assert.equal(w.options[0].detached, undefined); assert.equal(win.counts.unreaped, 1); assert.deepEqual(w.started, ["/t/a"]);
  assert.match(win.byCrate.a.detail, /Windows gives no proof for the processes it started/);

  // A stop request that failed or hung is unreaped at once, with the reason.
  const f = fake();
  const failed = await runTests({ ...base, spawn: f.spawn, stop: () => ({ ok: false, detail: "taskkill did not finish (ETIMEDOUT)" }), gone: () => true, platform: "win32" });
  assert.equal(failed.counts.unreaped, 1); assert.match(failed.byCrate.a.detail, /not proven stopped \(taskkill did not finish \(ETIMEDOUT\)\)/);

  // stopTree: taskkill /T /F with a deadline on Windows, its exit read; the process group elsewhere, its failure read.
  const calls = [];
  assert.deepEqual(stopTree({ pid: 7 }, { platform: "win32", killMs: 1234, run: (...x) => { calls.push(x); return { status: 0 }; } }), { ok: true, detail: "" });
  assert.deepEqual(calls[0].slice(0, 2), ["taskkill", ["/T", "/F", "/PID", "7"]]); assert.equal(calls[0][2].timeout, 1234);
  assert.equal(stopTree({ pid: 7 }, { platform: "win32", run: () => ({ status: 128 }) }).ok, true);
  assert.match(stopTree({ pid: 7 }, { platform: "win32", run: () => ({ status: 1 }) }).detail, /taskkill exited 1/);
  assert.match(stopTree({ pid: 7 }, { platform: "win32", run: () => ({ status: null, error: Object.assign(new Error("x"), { code: "ETIMEDOUT" }) }) }).detail, /did not finish \(ETIMEDOUT\)/);
  const sig = []; assert.equal(stopTree({ pid: 7 }, { platform: "linux", kill: (pid, s) => sig.push([pid, s]) }).ok, true);
  assert.deepEqual(sig, [[-7, "SIGKILL"]]);
  const err = (code) => () => { throw Object.assign(new Error(code), { code }); };
  assert.equal(stopTree({ pid: 7 }, { platform: "linux", kill: err("ESRCH") }).ok, true);
  assert.match(stopTree({ pid: 7 }, { platform: "linux", kill: err("EPERM") }).detail, /could not be signalled \(EPERM\)/);
  // treeGone: only "no such process" is proof; a live group and a refused signal are not; Windows never has proof here.
  assert.equal(treeGone({ pid: 7 }, { platform: "linux", kill: err("ESRCH") }), true);
  assert.equal(treeGone({ pid: 7 }, { platform: "linux", kill: err("EPERM") }), false);
  assert.equal(treeGone({ pid: 7 }, { platform: "linux", kill: () => true }), false);
  assert.equal(treeGone({ pid: 7 }, { platform: "win32" }), null);
});

test("G17: after a test program that is not proven stopped, no further Cargo call is sent and no crate is called passed", async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "rk-g17-"));
  try {
    for (const name of ["a", "b"]) { fs.mkdirSync(path.join(dir, "crates", name, "src"), { recursive: true }); fs.writeFileSync(path.join(dir, "crates", name, "Cargo.toml"), `[package]\nname = "${name}"\nversion = "0.1.0"\n`); fs.writeFileSync(path.join(dir, "crates", name, "src", "lib.rs"), ""); }
    const receipt = path.join(dir, "receipt.json"); fs.writeFileSync(receipt, JSON.stringify({ testExecutables: [] }));
    const sent = [];
    const send = (call) => { sent.push(call.stage);
      const lines = []; for (const c of call.crates) for (const t of call.targets ?? [targetsFor().hostTriple]) lines.push(JSON.stringify({ reason: "compiler-artifact", package_id: `path+file:///s/crates/${c.name}#0.1.0`, target: { kind: ["lib"], name: c.name }, filenames: [`/t/${t}/lib${c.name}.rlib`] }));
      lines.push(JSON.stringify({ reason: "build-finished", success: true }));
      return { status: 0, stdout: lines.join("\n"), stderr: "", requestId: "r", receipt, receiptSha256: "x" };
    };
    const unsettled = []; const lines = [];
    const run = await compileTogether({ snapshot: dir, commit: "c".repeat(40), crates: ["a", "b"], client: null, env: {}, verdictFile: path.join(dir, "v.json"), storeDir: null, send, out: (l) => lines.push(l), onUnsettled: (call, why) => unsettled.push(why),
      runTestsWith: async ({ crates }) => ({ byCrate: Object.fromEntries(crates.map((c) => [c.name, { result: c.name === "a" ? "failed" : "unknown", detail: "x", programs: 1, passed: 0, failed: 0, ignored: 0 }])), counts: { programs: 2, ran: 1, cached: 0, unreaped: 1 } }) });
    const at = sent.indexOf("test-build");
    assert.ok(at >= 0, "a test build was sent"); assert.equal(sent.length, at + 1, `calls were sent after the unreaped test: ${sent.slice(at + 1).join(", ")}`);
    assert.equal(unsettled.length, 1); assert.match(unsettled[0].reason, /not proven stopped/);
    assert.equal(run.code, 1); assert.equal(run.counts.tests.unreaped, 1);
    assert.equal(run.byCrate.a.result, "failed"); assert.equal(run.byCrate.b.result, "unknown"); assert.match(run.byCrate.b.detail, /stopped after .*not proven stopped; \d+ call\(s\) not sent/);
    assert.ok(lines.some((l) => /^stopped after /.test(l)));
  } finally { fs.rmSync(dir, { recursive: true, force: true }); }
});
