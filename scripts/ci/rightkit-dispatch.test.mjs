import assert from "node:assert/strict";
import test from "node:test";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { GH_TIMEOUT_MS, PROBE_TIMEOUT_MS, QUALIFICATION_SCHEMA, dispatchInputs, ghEnvironment, main, matchRun, matchingRuns, parseArgs, readQualification } from "./rightkit-dispatch.mjs";

const SHA = "a".repeat(40);
const OTHER = "b".repeat(40);
/** The gate's qualification receipt for rightkit-daemon at SHA, as rightkit-gate --to release writes it. */
/** One broker job receipt as the gate embeds it (id + signed envelope); the adapter forwards it untouched, the hosted admission verifies it. */
const RECEIPT = (id) => ({ id, provenance: { schema: "rustright.provenance.v1", payload: { jobId: id }, signature: { alg: "ES256", keyId: "k", value: "ab" } } });
const QUAL = { schema: "rightkit.unit-qualification/1", unit: { kind: "cargo", name: "rightkit-daemon", version: "0.4.0" }, commit: SHA, tree: "c".repeat(40), runtime: "r".repeat(64), executor: "broker",
  stages: { check: { ok: true }, compile: { ok: true, receipts: [RECEIPT("job-compile")] }, package: { ok: true, artifactSha256: "d".repeat(64), receipts: [RECEIPT("job-package")] }, qualify: { ok: true, artifactSha256: "d".repeat(64) } }, artifactSha256: "d".repeat(64) };
const QFILE = path.join(fs.mkdtempSync(path.join(os.tmpdir(), "rk-dispatch-qual-")), "qualification.json");
fs.writeFileSync(QFILE, JSON.stringify(QUAL));
const FORWARDED = { schema: QUAL.schema, commit: SHA, tree: QUAL.tree, runtime: QUAL.runtime, executor: "broker", unit: QUAL.unit, stages: { check: { ok: true }, compile: { ok: true, receipts: [RECEIPT("job-compile")] }, package: { ok: true, receipts: [RECEIPT("job-package")] }, qualify: { ok: true } }, artifactSha256: QUAL.artifactSha256 };
const admitted = async () => {};
const env = { RUSTRIGHT_JOB_ID: "req-123", GH_TOKEN: "not-printed" };

function fakeGh({ mainSha = SHA, runs = [], dispatchOk = true, store = { refs: new Map(), tags: new Map(), n: 0 }, claimsOk = true } = {}) {
  const calls = [];
  const field = (args, key) => { const i = args.findIndex((a, j) => a === "-f" && String(args[j + 1]).startsWith(`${key}=`)); return i < 0 ? undefined : args[i + 1].slice(key.length + 1); };
  const gh = (args) => {
    calls.push(args);
    const route = args.join(" ");
    if (route.includes("/commits/main")) return { ok: true, stdout: `${mainSha}\n` };
    if (route.includes("/dispatches")) return dispatchOk ? { ok: true, stdout: "" } : { ok: false, stdout: "" };
    if (route.includes("/actions/runs?")) return { ok: true, stdout: JSON.stringify({ workflow_runs: runs }) };
    const one = /\/actions\/runs\/(\d+)$/.exec(route);
    if (one) { const r = runs.find((x) => String(x.id) === one[1]); return r ? { ok: true, stdout: JSON.stringify(r) } : { ok: false, stdout: "" }; }
    // The shared claim namespace (dispatch-claim.mjs): an in-memory GitHub refs and annotated-tag store; ref creation fails when the ref exists.
    if (!claimsOk && /\/git\//.test(route)) return { ok: false, stdout: "" };
    const matching = /\/git\/matching-refs\/(.+)$/.exec(route);
    if (matching) return { ok: true, stdout: JSON.stringify([...store.refs].filter(([ref]) => `${ref}`.startsWith(`refs/${matching[1]}`)).map(([ref, sha]) => ({ ref, object: { sha, type: "tag" } }))) };
    const tagGet = /\/git\/tags\/([0-9a-f]+)$/.exec(route);
    if (tagGet) return store.tags.has(tagGet[1]) ? { ok: true, stdout: JSON.stringify({ message: store.tags.get(tagGet[1]) }) } : { ok: false, stdout: "" };
    if (route.includes("-X POST") && route.endsWith("") && /\/git\/tags /.test(route)) { const sha = (store.n += 1).toString(16).padStart(40, "0"); store.tags.set(sha, field(args, "message")); return { ok: true, stdout: JSON.stringify({ sha }) }; }
    if (route.includes("-X POST") && /\/git\/refs /.test(route)) { const ref = field(args, "ref"); if (store.refs.has(ref)) return { ok: false, stdout: "" }; store.refs.set(ref, field(args, "sha")); return { ok: true, stdout: "{}" }; }
    if (route.includes("-X PATCH") && /\/git\/refs\//.test(route)) { const ref = `refs/${/\/git\/refs\/(\S+)/.exec(route)[1]}`; if (!store.refs.has(ref)) return { ok: false, stdout: "" }; store.refs.set(ref, field(args, "sha")); return { ok: true, stdout: "{}" }; }
    throw new Error(`unexpected gh ${route}`);
  };
  return { gh, calls, store };
}

const run = (overrides = {}) => ({ id: 42, run_attempt: 1, head_sha: SHA, event: "workflow_dispatch", path: ".github/workflows/publish.yml", display_title: "publish crates rightkit-daemon req-123", html_url: "https://github.com/bogusyogi/rightkit/actions/runs/42", ...overrides });

test("argv: publish needs registry, one unit and a full sha; ci takes only sha", () => {
  assert.throws(() => parseArgs(["--workflow", "publish", "--sha", SHA]), /--registry/);
  assert.throws(() => parseArgs(["--workflow", "publish", "--registry", "crates", "--unit", "a b", "--sha", SHA]), /exactly one/);
  assert.throws(() => parseArgs(["--workflow", "publish", "--registry", "crates", "--unit", "rightkit-daemon", "--sha", "abc"]), /40-hex/);
  assert.throws(() => parseArgs(["--workflow", "ci", "--sha", SHA, "--unit", "x"]), /only --sha/);
  assert.throws(() => parseArgs(["--workflow", "nope", "--sha", SHA]), /ci or publish/);
  for (const inherited of ["constructor", "toString", "__proto__", "hasOwnProperty"]) assert.throws(() => parseArgs(["--workflow", inherited, "--sha", SHA]), /ci or publish/, `inherited key ${inherited} is not a workflow`);
  assert.throws(() => parseArgs(["--bogus"]), /unknown argument/);
  assert.throws(() => parseArgs(["--workflow", "publish", "--registry", "crates", "--unit", "rightkit-daemon", "--sha", SHA]), /--qualification <file> is required/);
  const opts = parseArgs(["--workflow", "publish", "--registry", "crates", "--unit", "rightkit-daemon", "--sha", SHA, "--qualification", QFILE, "--allow-ignored", "rightkit-daemon"]);
  assert.throws(() => dispatchInputs(opts, "req-123"), /carries the qualification receipt/);
  assert.deepEqual(dispatchInputs(opts, "req-123", FORWARDED), { request_id: "req-123", expected_sha: SHA, registry: "crates", units: "rightkit-daemon", qualification: JSON.stringify(FORWARDED), allow_host_exclusion: "", allow_no_test: "", allow_ignored: "rightkit-daemon", allow_missing_assets: "" });
  assert.deepEqual(dispatchInputs(parseArgs(["--workflow", "ci", "--sha", SHA]), "req-9"), { request_id: "req-9", expected_sha: SHA });
});

test("the child environment keeps the broker-injected HOME and forwards no token, no private gh config dir, nothing else", () => {
  const child = ghEnvironment({ GH_TOKEN: "t", HOME: "/h", GH_CONFIG_DIR: "/elsewhere", RIGHTKIT_ATTEST_NONCE: "n", AWS_SECRET: "s", PATH: "/usr/bin" });
  assert.equal(child.HOME, "/h");
  assert.equal(child.PATH, "/usr/bin");
  assert.equal(child.GH_PROMPT_DISABLED, "1");
  assert.equal(child.GH_TOKEN, undefined);
  assert.equal(child.GH_CONFIG_DIR, undefined);
  assert.equal(child.RIGHTKIT_ATTEST_NONCE, undefined);
  assert.equal(child.AWS_SECRET, undefined);
  assert.ok(GH_TIMEOUT_MS > 0 && PROBE_TIMEOUT_MS > 0 && PROBE_TIMEOUT_MS <= GH_TIMEOUT_MS, "every gh call is bounded");
});

test("refuses outside the broker: no RUSTRIGHT_JOB_ID", async () => {
  const lines = [];
  const code = await main(["--workflow", "ci", "--sha", SHA], { gh: fakeGh().gh, env: {}, stderr: (l) => lines.push(l), stdout: () => {} });
  assert.equal(code, 2);
  assert.match(lines.join("\n"), /RUSTRIGHT_JOB_ID/);
});

test("refuses when main moved after admission, before any dispatch", async () => {
  const { gh, calls } = fakeGh({ mainSha: OTHER });
  const lines = [];
  const code = await main(["--workflow", "publish", "--registry", "crates", "--unit", "rightkit-daemon", "--sha", SHA, "--qualification", QFILE], { gh, env, admission: admitted, stderr: (l) => lines.push(l), stdout: () => {} });
  assert.equal(code, 2);
  assert.match(lines.join("\n"), /main moved|refused, main is/);
  assert.ok(!calls.some((c) => c.includes("-X")), "no dispatch was posted");
});

test("dispatches with correlation inputs and discovers the run by its run-name", async () => {
  const { gh, calls } = fakeGh({ runs: [run({ id: 7, display_title: "publish crates rightkit-daemon req-999" }), run()] });
  const out = [];
  const code = await main(["--workflow", "publish", "--registry", "crates", "--unit", "rightkit-daemon", "--sha", SHA, "--qualification", QFILE], { gh, env, admission: admitted, stdout: (l) => out.push(l), stderr: () => {}, sleep: async () => {}, now: () => 0 });
  assert.equal(code, 0);
  const post = calls.find((c) => c.join(" ").includes("/dispatches"));
  assert.ok(post.includes("inputs[request_id]=req-123") && post.includes(`inputs[expected_sha]=${SHA}`) && post.includes("inputs[units]=rightkit-daemon") && post.includes("ref=main"));
  const record = JSON.parse(out[0]);
  assert.equal(record.runId, 42);
  assert.equal(record.requestId, "req-123");
  assert.equal(record.unit, "rightkit-daemon");
  assert.ok(!out.join("\n").includes("not-printed"), "no environment value is printed");
});

test("CI transport with injected admission posts only correlation inputs and matches the ci run, not a publish run of the same commit", async () => {
  const { gh, calls } = fakeGh({ runs: [run({ id: 1 }), run({ id: 2, path: ".github/workflows/ci.yml", display_title: "rightkit-shared-ci req-123" })] });
  const out = [];
  const code = await main(["--workflow", "ci", "--sha", SHA], { gh, env, admission: admitted, stdout: (l) => out.push(l), stderr: () => {}, sleep: async () => {}, now: () => 0 });
  assert.equal(code, 0);
  const post = calls.find((c) => c.join(" ").includes("/dispatches"));
  assert.ok(post.some((f) => f.endsWith("ci.yml/dispatches")) && !post.some((f) => f.includes("inputs[units]")));
  assert.equal(JSON.parse(out[0]).runId, 2);
});

test("a dispatch GitHub refuses, and a run that never appears, are reported and non-zero", async () => {
  assert.equal(await main(["--workflow", "ci", "--sha", SHA], { gh: fakeGh({ dispatchOk: false }).gh, env, admission: admitted, stdout: () => {}, stderr: () => {} }), 3);
  let t = 0;
  assert.equal(await main(["--workflow", "ci", "--sha", SHA], { gh: fakeGh({ runs: [] }).gh, env, admission: admitted, stdout: () => {}, stderr: () => {}, sleep: async () => { t += 30_000; }, now: () => t }), 2);
});

test("matchRun needs the same file, commit, event and the request id as an exact run-name token, exactly once", () => {
  const q = { workflow: "publish", sha: SHA, requestId: "req-123" };
  assert.equal(matchRun([run({ head_sha: OTHER })], q), null);
  assert.equal(matchRun([run({ event: "push" })], q), null);
  assert.equal(matchRun([run({ display_title: "publish crates rightkit-daemon req-124" })], q), null);
  assert.equal(matchRun([run({ display_title: "publish crates rightkit-daemon req-1234" })], q), null, "a longer id that contains ours is not ours");
  assert.equal(matchRun([run({ display_title: "publish crates rightkit-daemon xreq-123" })], q), null, "a substring inside another token is not a match");
  assert.equal(matchRun([run({ display_title: "publish\tcrates req-123\n" })], q).id, 42, "any whitespace delimits tokens");
  assert.equal(matchRun([run()], q).id, 42);
  assert.equal(matchRun([run({ id: 1 }), run({ id: 2 })], q), null, "two matching runs are ambiguous, never a guess");
  assert.deepEqual(matchingRuns([run({ id: 1 }), run({ id: 2 }), run({ id: 3, display_title: "other" })], q).map((r) => r.id), [1, 2]);
});

test("an ambiguous discovery is refused with exit 2 and no run id", async () => {
  const { gh } = fakeGh({ runs: [run({ id: 1 }), run({ id: 2 })] });
  const out = [], lines = [];
  assert.equal(await main(["--workflow", "publish", "--registry", "crates", "--unit", "rightkit-daemon", "--sha", SHA, "--qualification", QFILE], { gh, env, admission: admitted, stdout: (l) => out.push(l), stderr: (l) => lines.push(l), sleep: async () => {}, now: () => 0 }), 2);
  assert.deepEqual(out, []);
  assert.match(lines.join("\n"), /ambiguous, 2 publish.yml runs/);
});

test("invoked as a file, the entry guard runs main: outside the broker it refuses with exit 2 and prints nothing", () => {
  const script = fileURLToPath(new URL("./rightkit-dispatch.mjs", import.meta.url));
  const result = spawnSync(process.execPath, [script, "--probe"], { encoding: "utf8", env: { PATH: process.env.PATH }, timeout: 20_000 });
  assert.equal(result.status, 2);
  assert.equal(result.stdout, "");
  assert.match(result.stderr, /RUSTRIGHT_JOB_ID/);
});

test("probe requires the broker marker before gh is invoked, then prints only the boolean and the broker job id", async () => {
  let probes = 0;
  const probeGh = () => { probes += 1; return { ok: true, stdout: "" }; };
  const lines = [];
  assert.equal(await main(["--probe"], { gh: fakeGh().gh, probeGh, env: { GH_TOKEN: "not-printed" }, stdout: (l) => lines.push(l), stderr: (l) => lines.push(l) }), 2);
  assert.equal(probes, 0, "no gh call without RUSTRIGHT_JOB_ID");
  const out = [];
  assert.equal(await main(["--probe"], { gh: fakeGh().gh, probeGh, env, stdout: (l) => out.push(l), stderr: () => {} }), 0);
  assert.deepEqual(JSON.parse(out[0]), { authenticated: true, brokerJobId: "req-123" });
  const failed = [];
  assert.equal(await main(["--probe"], { gh: fakeGh().gh, probeGh: () => ({ ok: false, stdout: "" }), env, stdout: (l) => failed.push(l), stderr: (l) => failed.push(l) }), 3);
  assert.deepEqual(failed, [JSON.stringify({ authenticated: false, brokerJobId: "req-123" })], "no error payload beside the boolean");
  assert.ok(!(out.join("") + failed.join("")).includes("not-printed"));
});

test("dispatch-path failures name the step only; gh output never reaches stderr", async () => {
  const lines = [];
  const gh = (args) => args.join(" ").includes("/commits/main") ? { ok: false, stdout: "token-ish detail" } : { ok: true, stdout: "" };
  assert.equal(await main(["--workflow", "ci", "--sha", SHA], { gh, env, admission: admitted, stdout: () => {}, stderr: (l) => lines.push(l) }), 3);
  assert.deepEqual(lines, ["rightkit-dispatch: cannot read origin main through gh"]);
});


test("default admission refuses direct adapter use before POST when no valid local receipt exists", async () => {
  const { gh, calls } = fakeGh();
  const lines = [];
  const code = await main(["--workflow", "publish", "--registry", "crates", "--unit", "rightkit-daemon", "--sha", SHA, "--qualification", QFILE], { gh, env, stdout: () => {}, stderr: (l) => lines.push(l) });
  assert.equal(code, 2);
  assert.match(lines.join("\n"), /preflight refused/);
  assert.ok(!calls.some((c) => c.includes("-X")));
});

test("production admission refuses broad CI dispatch without scoped local proof", async () => {
  const { gh, calls } = fakeGh();
  const code = await main(["--workflow", "ci", "--sha", SHA], { gh, env, stdout: () => {}, stderr: () => {} });
  assert.equal(code, 2);
  assert.ok(!calls.some((c) => c.includes("-X")));
});

test("admission rejection never posts even when remote main matches", async () => {
  const { gh, calls } = fakeGh();
  const code = await main(["--workflow", "publish", "--registry", "crates", "--unit", "rightkit-daemon", "--sha", SHA, "--qualification", QFILE], { gh, env, admission: async () => { throw new Error("missing compile row"); }, stdout: () => {}, stderr: () => {} });
  assert.equal(code, 2);
  assert.ok(!calls.some((c) => c.includes("-X")));
});

test("the dispatcher's pinned helper digests match the helper files committed beside it (npm pre-flight, its imported pins module, select, crate pre-flight)", async () => {
  const { createHash } = await import("node:crypto");
  const fs = await import("node:fs");
  const here = new URL(".", import.meta.url);
  const source = fs.readFileSync(new URL("./rightkit-dispatch.mjs", here), "utf8");
  const pinned = (name) => new RegExp(`const ${name} = "([0-9a-f]{64})"`).exec(source)?.[1];
  const digest = (file) => createHash("sha256").update(fs.readFileSync(new URL(file, here))).digest("hex");
  assert.equal(pinned("PREFLIGHT_SHA256"), digest("./rk-preflight.mjs"));
  assert.equal(pinned("NPM_PREFLIGHT_SHA256"), digest("./rk-npm-preflight.mjs"));
  assert.equal(pinned("NPM_SELECT_SHA256"), digest("./select.mjs"));
  assert.equal(pinned("NPM_VERSION_PINS_SHA256"), digest("./release-version-pins.mjs"));
  assert.match(source, /release-version-pins\.mjs"\), NPM_VERSION_PINS_SHA256\]/, "the pins module is checked in the npm admission list");
});

test("the qualification receipt is re-read by the pinned adapter: commit, unit, every local stage ok, one artifact sha; only facts are forwarded", () => {
  const read = (over = {}) => readQualification(QFILE, { sha: SHA, unit: "rightkit-daemon", registry: "crates", readFile: () => JSON.stringify({ ...QUAL, ...over }) });
  assert.deepEqual(read(), FORWARDED);
  assert.throws(() => readQualification("/nope/qualification.json", { sha: SHA, unit: "rightkit-daemon", registry: "crates" }), /cannot read the qualification receipt/);
  assert.throws(() => read({ schema: "other" }), /schema/);
  assert.throws(() => read({ commit: OTHER }), /not the admitted/);
  assert.throws(() => readQualification(QFILE, { sha: SHA, unit: "rightkit-crash", registry: "crates" }), /covers cargo:rightkit-daemon, not cargo:rightkit-crash/);
  assert.throws(() => readQualification(QFILE, { sha: SHA, unit: "rightkit-daemon", registry: "npm" }), /not npm:rightkit-daemon/);
  assert.throws(() => read({ stages: { ...QUAL.stages, qualify: { ok: false } } }), /stage qualify is not ok/);
  assert.throws(() => read({ stages: { ...QUAL.stages, compile: undefined } }), /stage compile is not ok/);
  assert.throws(() => read({ stages: { ...QUAL.stages, compile: { ok: true } } }), /stage compile carries no signed broker receipts/);
  assert.throws(() => read({ stages: { ...QUAL.stages, package: { ...QUAL.stages.package, receipts: [] } } }), /stage package carries no signed broker receipts/);
  assert.equal(read().stages.compile.receipts[0].provenance.signature.value, "ab", "receipts are forwarded as the gate wrote them");
  assert.throws(() => read({ artifactSha256: "e".repeat(64) }), /one artifact sha256/);
  assert.throws(() => read({ tree: "short" }), /lacks a tree/);
  assert.doesNotMatch(JSON.stringify(read()), /\/Volumes|\/Users|[A-Z]:\\|qualification\.json/, "no host path of the gate's own travels to the workflow (the signed receipts carry the broker's, as signed)");
});

// ---- GB-47: the shared candidate claim at the dispatch boundary ----------------------------------------------------------------------------------

const PUBLISH = ["--workflow", "publish", "--registry", "crates", "--unit", "rightkit-daemon", "--sha", SHA, "--qualification", QFILE];
const qualFile = (over) => { const f = path.join(fs.mkdtempSync(path.join(os.tmpdir(), "rk-dispatch-claim-")), "q.json"); fs.writeFileSync(f, JSON.stringify({ ...QUAL, ...over, stages: { ...QUAL.stages, ...(over?.stages ?? {}) } })); return f; };
const origin = (id, { store, runs, qualification = QFILE, extra = {}, ok = true } = {}) => {
  const fake = fakeGh({ runs, store, dispatchOk: ok });
  const out = []; const err = [];
  const args = qualification === QFILE ? PUBLISH : PUBLISH.map((a) => (a === QFILE ? qualification : a));
  return { fake, out, err, run: () => main(args, { gh: fake.gh, env: { RUSTRIGHT_JOB_ID: id }, admission: admitted, stdout: (l) => out.push(l), stderr: (l) => err.push(l), sleep: async () => {}, now: (() => { let t = 0; return () => { t += 30_000; return t; }; })(), ...extra }) };
};
const dispatches = (fake) => fake.calls.filter((c) => c.join(" ").includes("/dispatches")).length;
const runFor = (id) => run({ id: 4000 + Number(id.slice(-1)), display_title: `publish broker ${id}` });

test("[GB-47] two origins race one candidate: exactly one dispatch is sent; the other joins the existing run (no second POST); the claim records the run", async () => {
  const store = { refs: new Map(), tags: new Map(), n: 0 };
  const mac = origin("req-m1", { store, runs: [runFor("req-m1")] });
  assert.equal(await mac.run(), 0, mac.err.join("\n"));
  assert.equal(dispatches(mac.fake), 1);
  const dell = origin("req-d1", { store, runs: [runFor("req-m1")] });
  assert.equal(await dell.run(), 0, dell.err.join("\n"));
  assert.equal(dispatches(dell.fake), 0, "the second origin sent nothing: it joined");
  const record = JSON.parse(dell.out[0]);
  assert.equal(record.joined, true); assert.equal(record.runId, 4001); assert.equal(record.requestId, "req-m1", "the joined run is the first origin's request");
  assert.equal(store.refs.size, 1, "one claim for the candidate");
  const claim = JSON.parse([...store.tags.values()].at(-1));
  assert.equal(claim.state, "dispatched"); assert.equal(claim.run.id, 4001);
  assert.match([...store.refs.keys()][0], /^refs\/rightkit-claims\/crates\/[0-9a-f]{16}\/0\.4\.0\/[0-9a-f]{64}$/);
});

test("[GB-47] different bytes (or a different source tree or policy) for the same version are refused before any send; the first claim stands", async () => {
  const store = { refs: new Map(), tags: new Map(), n: 0 };
  assert.equal(await origin("req-m1", { store, runs: [runFor("req-m1")] }).run(), 0);
  const otherBytes = qualFile({ artifactSha256: "e".repeat(64), stages: { package: { ...QUAL.stages.package, artifactSha256: "e".repeat(64) }, qualify: { ...QUAL.stages.qualify, artifactSha256: "e".repeat(64) } } });
  const dell = origin("req-d1", { store, runs: [runFor("req-m1")], qualification: otherBytes });
  assert.equal(await dell.run(), 2);
  assert.match(dell.err.join("\n"), /already claimed for different bytes, source or policy/);
  assert.equal(dispatches(dell.fake), 0);
  const policy = origin("req-d2", { store, runs: [runFor("req-m1")], extra: {} });
  const code = await main([...PUBLISH, "--allow-ignored", "rightkit-daemon"], { gh: policy.fake.gh, env: { RUSTRIGHT_JOB_ID: "req-d2" }, admission: admitted, stdout: () => {}, stderr: (l) => policy.err.push(l), sleep: async () => {}, now: () => 0 });
  assert.equal(code, 2, "another allow-set is another candidate"); assert.equal(dispatches(policy.fake), 0);
  assert.equal(store.refs.size, 1);
});

test("[GB-47] a lost acknowledgement (POST sent, no run seen) is reconciled by the claim's own token: the restarted origin finds the run and sends nothing; another origin waits instead of sending", async () => {
  const store = { refs: new Map(), tags: new Map(), n: 0 };
  const lost = origin("req-m1", { store, runs: [] });
  assert.equal(await lost.run(), 2, "no run appeared");
  assert.equal(dispatches(lost.fake), 1);
  assert.equal(JSON.parse([...store.tags.values()].at(-1)).state, "intent", "the send intent is persisted before the send");
  // Another origin while the first run is still invisible: it must not send a second dispatch.
  const dell = origin("req-d1", { store, runs: [] });
  assert.equal(await dell.run(), 2); assert.equal(dispatches(dell.fake), 0);
  assert.match(dell.err.join("\n"), /claimed by request req-m1 .* no run of it is visible yet/);
  // The run appears; the restarted origin (a new broker request) reconciles it without another POST and records it on the claim.
  const restarted = origin("req-m2", { store, runs: [runFor("req-m1")] });
  assert.equal(await restarted.run(), 0, restarted.err.join("\n"));
  assert.equal(dispatches(restarted.fake), 0, "no second POST");
  assert.equal(JSON.parse(restarted.out[0]).runId, 4001);
  assert.equal(JSON.parse([...store.tags.values()].at(-1)).state, "dispatched");
  // The same request retrying its own intent (no run yet) may send; that is the same send.
  const store2 = { refs: new Map(), tags: new Map(), n: 0 };
  await origin("req-m1", { store: store2, runs: [] }).run();
  const retry = origin("req-m1", { store: store2, runs: [] });
  await retry.run();
  assert.equal(dispatches(retry.fake), 1, "the same request id is a retry of the same send");
});

test("[GB-47] the claim namespace unreadable or unwritable fails closed (nothing sent); a claim naming a run the API does not confirm is not joined", async () => {
  const down = origin("req-m1", { store: { refs: new Map(), tags: new Map(), n: 0 }, runs: [runFor("req-m1")] });
  const closed = fakeGh({ claimsOk: false, runs: [runFor("req-m1")] });
  const out = []; const err = [];
  assert.equal(await main(PUBLISH, { gh: closed.gh, env: { RUSTRIGHT_JOB_ID: "req-m1" }, admission: admitted, stdout: (l) => out.push(l), stderr: (l) => err.push(l), sleep: async () => {}, now: () => 0 }), 3);
  assert.match(err.join("\n"), /claim unavailable/);
  assert.equal(dispatches(closed), 0);
  void down;
  const store = { refs: new Map(), tags: new Map(), n: 0 };
  assert.equal(await origin("req-m1", { store, runs: [runFor("req-m1")] }).run(), 0);
  const unconfirmed = origin("req-d1", { store, runs: [] });
  assert.equal(await unconfirmed.run(), 2);
  assert.match(unconfirmed.err.join("\n"), /which the Actions API does not confirm/);
  assert.equal(dispatches(unconfirmed.fake), 0);
});

