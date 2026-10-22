import assert from "node:assert/strict";
import test from "node:test";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { GH_TIMEOUT_MS, PROBE_TIMEOUT_MS, dispatchInputs, ghEnvironment, main, matchRun, matchingRuns, parseArgs } from "./rightkit-dispatch.mjs";

const SHA = "a".repeat(40);
const OTHER = "b".repeat(40);
const admitted = async () => {};
const env = { RIGHTKIT_JOB_ID: "req-123", GH_TOKEN: "not-printed" };

function fakeGh({ mainSha = SHA, runs = [], dispatchOk = true } = {}) {
  const calls = [];
  const gh = (args) => {
    calls.push(args);
    const route = args.join(" ");
    if (route.includes("/commits/main")) return { ok: true, stdout: `${mainSha}\n` };
    if (route.includes("/dispatches")) return dispatchOk ? { ok: true, stdout: "" } : { ok: false, stdout: "" };
    if (route.includes("/actions/runs?")) return { ok: true, stdout: JSON.stringify({ workflow_runs: runs }) };
    throw new Error(`unexpected gh ${route}`);
  };
  return { gh, calls };
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
  const opts = parseArgs(["--workflow", "publish", "--registry", "crates", "--unit", "rightkit-daemon", "--sha", SHA, "--allow-ignored", "rightkit-daemon"]);
  assert.deepEqual(dispatchInputs(opts, "req-123"), { request_id: "req-123", expected_sha: SHA, registry: "crates", units: "rightkit-daemon", allow_host_exclusion: "", allow_no_test: "", allow_ignored: "rightkit-daemon", allow_missing_assets: "" });
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

test("refuses outside the broker: no RIGHTKIT_JOB_ID", async () => {
  const lines = [];
  const code = await main(["--workflow", "ci", "--sha", SHA], { gh: fakeGh().gh, env: {}, stderr: (l) => lines.push(l), stdout: () => {} });
  assert.equal(code, 2);
  assert.match(lines.join("\n"), /RIGHTKIT_JOB_ID/);
});

test("refuses when main moved after admission, before any dispatch", async () => {
  const { gh, calls } = fakeGh({ mainSha: OTHER });
  const lines = [];
  const code = await main(["--workflow", "publish", "--registry", "crates", "--unit", "rightkit-daemon", "--sha", SHA], { gh, env, admission: admitted, stderr: (l) => lines.push(l), stdout: () => {} });
  assert.equal(code, 2);
  assert.match(lines.join("\n"), /main moved|refused, main is/);
  assert.ok(!calls.some((c) => c.includes("-X")), "no dispatch was posted");
});

test("dispatches with correlation inputs and discovers the run by its run-name", async () => {
  const { gh, calls } = fakeGh({ runs: [run({ id: 7, display_title: "publish crates rightkit-daemon req-999" }), run()] });
  const out = [];
  const code = await main(["--workflow", "publish", "--registry", "crates", "--unit", "rightkit-daemon", "--sha", SHA], { gh, env, admission: admitted, stdout: (l) => out.push(l), stderr: () => {}, sleep: async () => {}, now: () => 0 });
  assert.equal(code, 0);
  const post = calls.find((c) => c.includes("-X"));
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
  const post = calls.find((c) => c.includes("-X"));
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
  assert.equal(await main(["--workflow", "publish", "--registry", "crates", "--unit", "rightkit-daemon", "--sha", SHA], { gh, env, admission: admitted, stdout: (l) => out.push(l), stderr: (l) => lines.push(l), sleep: async () => {}, now: () => 0 }), 2);
  assert.deepEqual(out, []);
  assert.match(lines.join("\n"), /ambiguous, 2 publish.yml runs/);
});

test("invoked as a file, the entry guard runs main: outside the broker it refuses with exit 2 and prints nothing", () => {
  const script = fileURLToPath(new URL("./rightkit-dispatch.mjs", import.meta.url));
  const result = spawnSync(process.execPath, [script, "--probe"], { encoding: "utf8", env: { PATH: process.env.PATH }, timeout: 20_000 });
  assert.equal(result.status, 2);
  assert.equal(result.stdout, "");
  assert.match(result.stderr, /RIGHTKIT_JOB_ID/);
});

test("probe requires the broker marker before gh is invoked, then prints only the boolean and the broker job id", async () => {
  let probes = 0;
  const probeGh = () => { probes += 1; return { ok: true, stdout: "" }; };
  const lines = [];
  assert.equal(await main(["--probe"], { gh: fakeGh().gh, probeGh, env: { GH_TOKEN: "not-printed" }, stdout: (l) => lines.push(l), stderr: (l) => lines.push(l) }), 2);
  assert.equal(probes, 0, "no gh call without RIGHTKIT_JOB_ID");
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
  const code = await main(["--workflow", "publish", "--registry", "crates", "--unit", "rightkit-daemon", "--sha", SHA], { gh, env, stdout: () => {}, stderr: (l) => lines.push(l) });
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
  const code = await main(["--workflow", "publish", "--registry", "crates", "--unit", "rightkit-daemon", "--sha", SHA], { gh, env, admission: async () => { throw new Error("missing compile row"); }, stdout: () => {}, stderr: () => {} });
  assert.equal(code, 2);
  assert.ok(!calls.some((c) => c.includes("-X")));
});
