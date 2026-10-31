import assert from "node:assert/strict";
import test from "node:test";
import crypto from "node:crypto";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { CLAIM_STALE_MS, GH_TIMEOUT_MS, PROBE_TIMEOUT_MS, QUALIFICATION_SCHEMA, REPO, WORKFLOWS, claimIdentity, claimKey, dispatchInputs, ghEnvironment, main, matchRun, matchingRuns, parseArgs, readClaim, readQualification, requirePreflight, reserveClaim } from "./rightkit-dispatch.mjs";

const SHA = "a".repeat(40);
const OTHER = "b".repeat(40);
const TREE = "c".repeat(40);
/** The gate's qualification receipt for rightkit-daemon at SHA, as rightkit-gate --to release writes it. */
/** One broker job receipt as the gate embeds it (id + signed envelope); the adapter forwards it untouched, the hosted admission verifies it. */
const RECEIPT = (id) => ({ id, provenance: { schema: "rustright.provenance.v1", payload: { jobId: id }, signature: { alg: "ES256", keyId: "k", value: "ab" } } });
const QUAL = { schema: "rightkit.unit-qualification/1", unit: { kind: "cargo", name: "rightkit-daemon", version: "0.4.0" }, commit: SHA, tree: TREE, runtime: "r".repeat(64), executor: "broker",
  stages: { check: { ok: true }, compile: { ok: true, receipts: [RECEIPT("job-compile")] }, package: { ok: true, artifactSha256: "d".repeat(64), receipts: [RECEIPT("job-package")] }, qualify: { ok: true, artifactSha256: "d".repeat(64) } }, artifactSha256: "d".repeat(64) };
const QFILE = path.join(fs.mkdtempSync(path.join(os.tmpdir(), "rk-dispatch-qual-")), "qualification.json");
fs.writeFileSync(QFILE, JSON.stringify(QUAL));
const FORWARDED = { schema: QUAL.schema, commit: SHA, tree: QUAL.tree, runtime: QUAL.runtime, executor: "broker", unit: QUAL.unit, stages: { check: { ok: true }, compile: { ok: true, receipts: [RECEIPT("job-compile")] }, package: { ok: true, receipts: [RECEIPT("job-package")] }, qualify: { ok: true } }, artifactSha256: QUAL.artifactSha256 };
const admitted = async () => {};
const env = { RUSTRIGHT_JOB_ID: "req-123", GH_TOKEN: "not-printed" };

// An in-memory GitHub, modelled on the real API semantics the claim relies on: ref creation fails when the ref exists; a ref moves only by fast-forward
// when force=false; commits are content-addressed; matching-refs lists by prefix; the runs listing is paginated, can lag behind a POST, and can fail.
// `store` is the shared remote state, so several origins (one gh each) act on one GitHub. Per-origin options model that origin's own network view.
const newStore = () => ({ refs: new Map(), commits: new Map(), runs: [], preset: [], posts: [], listings: 0, runSeq: 5000, listingDown: false });
function fakeGh({ mainSha = SHA, runs, dispatchOk = true, dispatchMode = !dispatchOk ? "error" : runs ? "none" : "ok", store = newStore(), claimsOk = true, staleFirstRead = false, visibleAfterListings = 0, fillerRuns = 0, tree = TREE, listFailsAfterPost = false } = {}) {
  const calls = [];
  if (runs) store.preset = runs;
  let stale = staleFirstRead;
  const method = (args) => (args.includes("-X") ? args[args.indexOf("-X") + 1] : "GET");
  const fields = (args) => { const out = {}; for (let i = 0; i < args.length - 1; i += 1) if (args[i] === "-f" || args[i] === "-F") { const s = args[i + 1]; const at = s.indexOf("="); (out[s.slice(0, at)] ??= []).push(s.slice(at + 1)); } return out; };
  const ok = (body) => ({ ok: true, stdout: JSON.stringify(body) });
  const no = { ok: false, stdout: "" };
  const descends = (from, to) => { for (let c = to; c; c = store.commits.get(c)?.parents[0]) if (c === from) return true; return false; };
  const gh = (args) => {
    calls.push(args);
    const url = args.find((a) => a.startsWith("repos/")) ?? "";
    const verb = method(args);
    const f = fields(args);
    if (url.endsWith("/commits/main")) return { ok: true, stdout: `${mainSha}\n` };
    if (url.endsWith(`/git/commits/${SHA}`)) return tree === null ? no : ok({ sha: SHA, tree: { sha: tree } });
    const listing = /\/actions\/runs\?event=workflow_dispatch&head_sha=([0-9a-f]+)&per_page=(\d+)&page=(\d+)$/.exec(url);
    if (listing) {
      store.listings += 1;
      if (store.listingDown || (listFailsAfterPost && store.posts.length)) return no;
      const fillers = Array.from({ length: fillerRuns }, (_, i) => ({ id: 100000 + i, run_attempt: 1, head_sha: SHA, event: "workflow_dispatch", path: ".github/workflows/publish.yml", display_title: `unrelated ${i}` }));
      const all = [...fillers, ...store.runs.filter((r) => store.listings >= r.visibleAt).map(({ visibleAt, ...r }) => r), ...store.preset].filter((r) => r.head_sha === listing[1]);
      const per = Number(listing[2]); const page = Number(listing[3]);
      return ok({ total_count: all.length, workflow_runs: all.slice((page - 1) * per, page * per) });
    }
    const one = /\/actions\/runs\/(\d+)$/.exec(url);
    if (one) { const r = [...store.runs, ...store.preset].find((x) => String(x.id) === one[1]); if (!r) return no; const { visibleAt, ...rest } = r; return ok(rest); }
    const dispatch = /\/actions\/workflows\/([^/]+)\/dispatches$/.exec(url);
    if (dispatch && verb === "POST") {
      const input = (k) => f[`inputs[${k}]`]?.[0];
      store.posts.push({ workflow: dispatch[1], requestId: input("request_id"), args });
      if (dispatchMode !== "error" && dispatchMode !== "none") {
        const title = dispatch[1] === "ci.yml" ? `rightkit-shared-ci ${input("request_id")}` : `publish ${input("registry")} ${input("units")} ${input("request_id")}`;
        store.runSeq += 1;
        store.runs.push({ id: store.runSeq, run_attempt: 1, head_sha: input("expected_sha"), event: "workflow_dispatch", path: `.github/workflows/${dispatch[1]}`, display_title: title, html_url: `https://github.com/${REPO}/actions/runs/${store.runSeq}`, visibleAt: store.listings + visibleAfterListings });
      }
      return dispatchMode === "ok" || dispatchMode === "none" ? { ok: true, stdout: "" } : no;
    }
    if (url.includes("/git/")) {
      if (!claimsOk) return no;
      const commitGet = /\/git\/commits\/([0-9a-f]{40})$/.exec(url);
      if (commitGet && verb === "GET") { const c = store.commits.get(commitGet[1]); return c ? ok({ sha: commitGet[1], message: c.message, tree: { sha: c.tree }, parents: c.parents.map((sha) => ({ sha })) }) : no; }
      if (/\/git\/commits$/.test(url) && verb === "POST") {
        const c = { message: f.message[0], tree: f.tree[0], parents: f["parents[]"] ?? [], date: f["author[date]"]?.[0] };
        const sha = crypto.createHash("sha1").update(JSON.stringify(c)).digest("hex");
        store.commits.set(sha, c);
        return ok({ sha });
      }
      const matching = /\/git\/matching-refs\/([^?]+)/.exec(url);
      if (matching) {
        const entries = stale ? [] : [...store.refs].filter(([ref]) => ref.startsWith(`refs/${matching[1]}`)).map(([ref, sha]) => ({ ref, object: { sha, type: "commit" } }));
        stale = false;
        return ok(entries);
      }
      if (/\/git\/refs$/.test(url) && verb === "POST") {
        const ref = f.ref[0];
        if (store.refs.has(ref) || [...store.refs.keys()].some((r) => r.startsWith(`${ref}/`) || ref.startsWith(`${r}/`))) return no;
        store.refs.set(ref, f.sha[0]);
        return ok({ ref });
      }
      const patch = /\/git\/refs\/(.+)$/.exec(url);
      if (patch && verb === "PATCH") {
        assert.ok(args.includes("force=false"), "a claim swap is a conditional update: it must pass force=false");
        const ref = `refs/${patch[1]}`;
        const current = store.refs.get(ref);
        if (!current || !store.commits.has(f.sha[0]) || !descends(current, f.sha[0])) return no;
        store.refs.set(ref, f.sha[0]);
        return ok({ ref });
      }
    }
    throw new Error(`unexpected gh ${args.join(" ")}`);
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

test("a dispatch GitHub does not confirm, and a run that never appears, are reported and non-zero", async () => {
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

test("production admission refuses broad CI dispatch with the typed [ci-not-admitted] refusal, before any claim or POST", async () => {
  const { gh, calls } = fakeGh();
  const lines = [];
  const code = await main(["--workflow", "ci", "--sha", SHA], { gh, env, stdout: () => {}, stderr: (l) => lines.push(l) });
  assert.equal(code, 2);
  assert.match(lines.join("\n"), /preflight refused \[ci-not-admitted\]: only scoped crates\/npm publication has local admission/);
  assert.ok(!calls.some((c) => c.includes("-X")), "neither a claim nor a dispatch was written");
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

test("every ADMISSION_DEPENDENCIES pin equals the digest of the committed file it names (a stale pin would make the production admission refuse every dispatch)", () => {
  const root = fileURLToPath(new URL("../../", import.meta.url));
  const source = fs.readFileSync(new URL("./rightkit-dispatch.mjs", import.meta.url), "utf8");
  const table = /const ADMISSION_DEPENDENCIES = \{([\s\S]*?)\};/.exec(source)[1];
  const pins = [...table.matchAll(/"([^"]+)":\s*"([0-9a-f]{64})"/g)];
  assert.ok(pins.length >= 6);
  for (const [, rel, expected] of pins) assert.equal(crypto.createHash("sha256").update(fs.readFileSync(path.join(root, rel))).digest("hex"), expected, `${rel} changed: re-pin it in scripts/ci/rightkit-dispatch.mjs`);
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

// ---- RA-19 / GB-47: the digest-pinned dispatcher's executable closure is the one file ------------------------------------------------------------

test("[RA-19] the dispatcher has no static relative import: its executable closure is itself plus node built-ins, so the broker's pin covers all of it", () => {
  const source = fs.readFileSync(new URL("./rightkit-dispatch.mjs", import.meta.url), "utf8");
  const staticImports = [...source.matchAll(/^\s*(?:import|export)\s[^;\n]*?\sfrom\s+["']([^"']+)["']/gm)].map((m) => m[1]);
  assert.ok(staticImports.length > 0);
  for (const specifier of staticImports) assert.match(specifier, /^node:/, `static import ${specifier} must be a node built-in`);
  assert.doesNotMatch(source, /^\s*import\s*\(/m);
  assert.doesNotMatch(source, /dispatch-claim\.mjs["']/, "the dispatcher never names the re-export shim as a module specifier");
});

test("[RA-19] an installed-policy copy of the dispatcher alone (no checkout siblings, no dispatch-claim.mjs) imports and runs its entry guard", () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "rk-dispatch-installed-"));
  const copy = path.join(dir, "rightkit-dispatch.mjs");
  fs.copyFileSync(fileURLToPath(new URL("./rightkit-dispatch.mjs", import.meta.url)), copy);
  assert.deepEqual(fs.readdirSync(dir), ["rightkit-dispatch.mjs"]);
  const result = spawnSync(process.execPath, [copy, "--probe"], { encoding: "utf8", env: { PATH: process.env.PATH }, cwd: dir, timeout: 20_000 });
  assert.equal(result.status, 2, result.stderr);
  assert.equal(result.stdout, "");
  assert.match(result.stderr, /RUSTRIGHT_JOB_ID/, "the module evaluated and main refused for the missing broker marker (a missing sibling would be ERR_MODULE_NOT_FOUND)");
});

test("[RA-19] tampering any admission helper or dependency is refused BEFORE any of that code is evaluated (a sentinel module would write a marker)", async () => {
  const repoRoot = fileURLToPath(new URL("../../", import.meta.url));
  const source = fs.readFileSync(new URL("./rightkit-dispatch.mjs", import.meta.url), "utf8");
  const table = /const ADMISSION_DEPENDENCIES = \{([\s\S]*?)\};/.exec(source)[1];
  const files = ["scripts/ci/rk-preflight.mjs", ...[...table.matchAll(/"([^"]+)":\s*"[0-9a-f]{64}"/g)].map((m) => m[1])];
  assert.ok(files.length >= 7);
  const opts = { workflow: "publish", registry: "crates", unit: "rightkit-daemon", sha: SHA, allow: {} };
  const cwd = process.cwd();
  const stage = (tamper) => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), "rk-dispatch-tamper-"));
    fs.mkdirSync(path.join(root, ".git"));
    for (const rel of files) { fs.mkdirSync(path.dirname(path.join(root, rel)), { recursive: true }); fs.copyFileSync(path.join(repoRoot, rel), path.join(root, rel)); }
    const marker = path.join(root, "EXECUTED");
    if (tamper) fs.writeFileSync(path.join(root, tamper), `import fs from "node:fs"; fs.writeFileSync(${JSON.stringify(marker)}, "evaluated");\n`);
    return { root, marker };
  };
  try {
    for (const tampered of files) {
      const { root, marker } = stage(tampered);
      process.chdir(root);
      await assert.rejects(requirePreflight(opts), /digest changed/, `${tampered} tampered`);
      assert.ok(!fs.existsSync(marker), `${tampered}: the tampered bytes were evaluated before the digest check refused them`);
    }
    const { root } = stage(null);
    process.chdir(root);
    await assert.rejects(requirePreflight(opts), (error) => !/digest changed/.test(error.message), "untouched bytes pass the digest checks and are refused only later, by the git proof");
  } finally { process.chdir(cwd); }
});

// ---- GB-47 / RA-04 / RA-05: the shared candidate claim at the dispatch boundary ----------------------------------------------------------------

const PUBLISH = ["--workflow", "publish", "--registry", "crates", "--unit", "rightkit-daemon", "--sha", SHA, "--qualification", QFILE];
const qualFile = (over) => { const f = path.join(fs.mkdtempSync(path.join(os.tmpdir(), "rk-dispatch-claim-")), "q.json"); fs.writeFileSync(f, JSON.stringify({ ...QUAL, ...over, stages: { ...QUAL.stages, ...(over?.stages ?? {}) } })); return f; };
const OTHER_BYTES = () => qualFile({ artifactSha256: "e".repeat(64), stages: { package: { ...QUAL.stages.package, artifactSha256: "e".repeat(64) }, qualify: { ...QUAL.stages.qualify, artifactSha256: "e".repeat(64) } } });
const newClock = () => { const c = { t: Date.parse("2026-10-31T00:00:00Z") }; return { c, now: () => c.t, sleep: async (ms) => { c.t += ms; } }; };
/** One origin: its own gh view of the shared store, its own broker request id, a shared clock. */
const origin = (id, store, { argv = PUBLISH, qualification, time = newClock(), gh = {}, deps = {} } = {}) => {
  const fake = fakeGh({ store, ...gh });
  const out = []; const err = [];
  const args = qualification ? argv.map((a) => (a === QFILE ? qualification : a)) : argv;
  return { fake, out, err, time, run: () => main(args, { gh: fake.gh, env: { RUSTRIGHT_JOB_ID: id }, admission: admitted, stdout: (l) => out.push(l), stderr: (l) => err.push(l), sleep: time.sleep, now: time.now, ...deps }) };
};
const posts = (store) => store.posts.length;
const claimKeyRef = () => `refs/${claimKey({ registry: "crates", unit: "rightkit-daemon", version: "0.4.0" })}`;
const claimOf = (store, ref = claimKeyRef()) => JSON.parse(store.commits.get(store.refs.get(ref)).message);
const pendingLine = (o) => o.out.map((l) => JSON.parse(l)).find((r) => r.pending);
const IDENTITY = (over = {}) => claimIdentity({ repo: REPO, workflow: WORKFLOWS.publish, sha: SHA, tree: TREE, registry: "crates", unit: "rightkit-daemon", version: "0.4.0", artifactSha256: "d".repeat(64), allow: {}, ...over });
const inert = { discover: () => ({ ok: true, complete: true, runs: [] }), verifyRun: () => ({ status: "unconfirmed" }) };

test("[GB-47] one candidate, two origins in turn: exactly one POST; the second joins the recorded run; ONE shared key holds the digest as a value, never in its name", async () => {
  const store = newStore();
  const mac = origin("req-m1", store);
  assert.equal(await mac.run(), 0, mac.err.join("\n"));
  assert.equal(posts(store), 1);
  const dell = origin("req-d1", store);
  assert.equal(await dell.run(), 0, dell.err.join("\n"));
  assert.equal(posts(store), 1, "the second origin sent nothing: it joined");
  const record = JSON.parse(dell.out[0]);
  assert.equal(record.joined, true); assert.equal(record.runId, 5001); assert.equal(record.requestId, "req-m1", "the joined run is the first origin's request");
  assert.deepEqual([...store.refs.keys()], [claimKeyRef()]);
  assert.match(claimKeyRef(), /^refs\/rightkit-claims\/crates\/[0-9a-f]{16}\/0\.4\.0$/);
  const claim = claimOf(store);
  assert.equal(claim.state, "dispatched"); assert.equal(claim.run.id, 5001); assert.equal(claim.identity.tree, TREE); assert.equal(claim.identity.artifactSha256, "d".repeat(64));
  assert.match(claim.identityDigest, /^[0-9a-f]{64}$/); assert.ok(!claimKeyRef().includes(claim.identityDigest));
  const tip = store.commits.get(store.refs.get(claimKeyRef()));
  assert.equal(tip.tree, TREE, "the claim commit is bound to the proved source tree");
});

test("[RA-04] both callers finish their empty-list reads before either creates a claim: DIFFERENT digests, one reservation and one POST, the other is refused", async () => {
  // Sequential origins with a forced stale first read model the interleaving read(A) read(B) create(A) create(B): B's listing is empty although A then creates.
  const store = newStore();
  const a = origin("req-m1", store, { gh: { staleFirstRead: true } });
  const b = origin("req-d1", store, { qualification: OTHER_BYTES(), gh: { staleFirstRead: true } });
  const codes = await Promise.all([a.run(), b.run()]);
  assert.deepEqual(codes, [0, 2]);
  assert.equal(posts(store), 1, "exactly one POST");
  assert.equal(store.refs.size, 1, "exactly one reservation: one conflict key per registry/package/version");
  assert.match(b.err.join("\n"), /refused, conflicting candidate: \[claim-conflict\] rightkit-daemon@0\.4\.0 is already claimed for different bytes, source or policy/);
  assert.equal(claimOf(store).requestId, "req-m1", "the first claim stands");
  // Another allow-set and another tree are other candidates of the same version: refused too.
  const policy = origin("req-d2", store, { argv: [...PUBLISH, "--allow-ignored", "rightkit-daemon"] });
  assert.equal(await policy.run(), 2); assert.equal(posts(store), 1);
});

test("[RA-04] both callers finish their empty-list reads before either creates a claim: SAME digest, exactly one dispatch, the other joins", async () => {
  const store = newStore();
  const a = origin("req-m1", store, { gh: { staleFirstRead: true } });
  const b = origin("req-d1", store, { gh: { staleFirstRead: true } });
  const codes = await Promise.all([a.run(), b.run()]);
  assert.deepEqual(codes, [0, 0], a.err.concat(b.err).join("\n"));
  assert.equal(posts(store), 1);
  assert.equal(store.refs.size, 1);
  assert.equal(JSON.parse(b.out[0]).joined, true);
  assert.equal(JSON.parse(b.out[0]).runId, JSON.parse(a.out[0]).runId);
});

test("[RA-05] the same request retried concurrently (both read empty) produces one effect", async () => {
  const store = newStore();
  const a = origin("req-m1", store, { gh: { staleFirstRead: true } });
  const b = origin("req-m1", store, { gh: { staleFirstRead: true } });
  const codes = await Promise.all([a.run(), b.run()]);
  assert.deepEqual(codes, [0, 0], a.err.concat(b.err).join("\n"));
  assert.equal(posts(store), 1);
});

test("[RA-04] an unreadable or corrupt claim is a conflict (never absent); an unreadable namespace is unavailable; nothing is sent in either case", async () => {
  const seed = (message, { dangling = false } = {}) => {
    const store = newStore();
    const sha = crypto.createHash("sha1").update(String(message)).digest("hex");
    if (!dangling) store.commits.set(sha, { message, tree: TREE, parents: [], date: "x" });
    store.refs.set(claimKeyRef(), sha);
    return store;
  };
  for (const message of ["not json at all", JSON.stringify({ schema: "other" }), JSON.stringify({ schema: "rightkit.dispatch-claim/2", key: "x" })]) {
    const store = seed(message);
    const o = origin("req-m1", store);
    assert.equal(await o.run(), 2, message);
    assert.match(o.err.join("\n"), /\[claim-unreadable\]/);
    assert.equal(posts(store), 0);
  }
  // A claim whose identity does not hash to its digest has been tampered with.
  const real = origin("req-x", newStore()); await real.run();
  const tampered = { ...claimOf(real.fake.store), identityDigest: "f".repeat(64) };
  const t = seed(JSON.stringify(tampered)); const to = origin("req-m1", t);
  assert.equal(await to.run(), 2); assert.match(to.err.join("\n"), /its identity does not match its digest/); assert.equal(posts(t), 0);
  // A claim ref whose commit cannot be read right now is "cannot prove", not "absent".
  const dangling = seed("{}", { dangling: true }); const d = origin("req-m1", dangling);
  assert.equal(await d.run(), 3); assert.match(d.err.join("\n"), /claim unavailable: \[claim-unavailable\]/); assert.equal(posts(dangling), 0);
  const down = origin("req-m1", newStore(), { gh: { claimsOk: false } });
  assert.equal(await down.run(), 3); assert.match(down.err.join("\n"), /claim unavailable/); assert.equal(posts(down.fake.store), 0);
  // A retired per-digest layout under the same version is not silently ignored.
  const legacy = newStore(); legacy.refs.set(`${claimKeyRef()}/${"9".repeat(64)}`, "9".repeat(40));
  const l = origin("req-m1", legacy);
  assert.equal(await l.run(), 2); assert.match(l.err.join("\n"), /\[claim-legacy\]/); assert.equal(posts(legacy), 0);
});

test("[RA-04] stale-owner reconciliation: a reserved claim is waited for, then reclaimed by swap after the stale window; the old owner then cannot send", async () => {
  const store = newStore();
  const time = newClock();
  const gh = fakeGh({ store }).gh;
  const base = { gh, env: {}, repo: REPO, identity: IDENTITY(), now: time.now, ...inert };
  const dead = reserveClaim({ ...base, requestId: "req-dead", attempt: "attempt-dead" });
  assert.equal(dead.status, "reserved");
  // Another request inside the window: pending, nothing sent (the owner may be about to send).
  const early = origin("req-live", store, { time });
  assert.equal(await early.run(), 2);
  assert.match(early.err.join("\n"), /\[claim-reserved-elsewhere\]/);
  assert.equal(posts(store), 0);
  assert.equal(pendingLine(early).pending, true);
  // After the stale window the live request reclaims it by a conditional swap and sends exactly once.
  time.c.t += CLAIM_STALE_MS + 1;
  const late = origin("req-live", store, { time });
  assert.equal(await late.run(), 0, late.err.join("\n"));
  assert.equal(posts(store), 1);
  assert.equal(store.posts[0].requestId, "req-live");
  assert.equal(claimOf(store).takeover.requestId, "req-dead");
  // The old owner wakes up: its send-intent swap fails (the ref moved), so it can never POST.
  assert.equal(dead.beginSend(), false);
  // A retry of the same request id reclaims its own earlier reserved claim at once (a restart before sending); two attempts of one request cannot both send.
  const store2 = newStore();
  const g2 = fakeGh({ store: store2 }).gh;
  const first = reserveClaim({ gh: g2, env: {}, repo: REPO, identity: IDENTITY(), now: time.now, requestId: "req-r", attempt: "a1", ...inert });
  const second = reserveClaim({ gh: g2, env: {}, repo: REPO, identity: IDENTITY(), now: time.now, requestId: "req-r", attempt: "a2", ...inert });
  assert.equal(second.status, "reserved");
  assert.equal(first.beginSend(), false, "the superseded attempt cannot persist a send intent");
  assert.equal(second.beginSend(), true);
});

test("[RA-05] accepted POST, response dropped, listing delayed: one effect and an explicit pending state; the same request retried never POSTs again and joins once the run is visible", async () => {
  const store = newStore();
  const first = origin("req-m1", store, { gh: { dispatchMode: "drop-response", visibleAfterListings: 2 } });
  assert.equal(await first.run(), 3, first.err.join("\n"));
  assert.equal(posts(store), 1);
  assert.equal(claimOf(store).state, "sending", "the send intent was persisted before the POST and is not cleared by a failure");
  assert.equal(pendingLine(first).code, "claim-sent-or-unknown");
  assert.match(first.err.join("\n"), /may still have been accepted/);
  // The restarted request (same id) and a different origin, while the run is still invisible: both pending, neither sends.
  const hiddenStore = newStore();
  const lost = origin("req-m1", hiddenStore, { gh: { dispatchMode: "drop-response", visibleAfterListings: 1000 } });
  assert.equal(await lost.run(), 3);
  const sameId = origin("req-m1", hiddenStore, { gh: { visibleAfterListings: 1000 } });
  assert.equal(await sameId.run(), 2); assert.match(sameId.err.join("\n"), /\[claim-sent-or-unknown\] .*claimed by request req-m1 .* no run of it is visible yet/);
  const other = origin("req-d1", hiddenStore);
  assert.equal(await other.run(), 2);
  assert.equal(posts(hiddenStore), 1, "pending until authoritative reconciliation: no second POST by anyone");
  assert.equal(claimOf(hiddenStore).state, "sending");
  // Visible now: the retry reconciles by the effect token, sends nothing, and records the run on the claim.
  const retry = origin("req-m1", store);
  assert.equal(await retry.run(), 0, retry.err.join("\n"));
  assert.equal(posts(store), 1, "no second POST");
  assert.equal(JSON.parse(retry.out[0]).runId, 5001); assert.equal(JSON.parse(retry.out[0]).joined, true);
  assert.equal(claimOf(store).state, "dispatched");
});

test("[RA-05] POST error: pending, never re-sent; a failed run record write does not enable a resend", async () => {
  const store = newStore();
  const failed = origin("req-m1", store, { gh: { dispatchMode: "error" } });
  assert.equal(await failed.run(), 3);
  assert.equal(pendingLine(failed).accepted, false);
  const retry = origin("req-m1", store);
  assert.equal(await retry.run(), 2);
  assert.equal(posts(store), 1, "an API error is not proof of non-dispatch");
  // The run is created and seen, but the swap that records it is refused: the claim stays at "sending" and still blocks a resend.
  const s2 = newStore();
  const flaky = fakeGh({ store: s2 });
  const gh = (args) => (args.includes("PATCH") && args.includes("force=false") && claimOf(s2).state === "sending" ? { ok: false, stdout: "" } : flaky.gh(args));
  const o = []; const e = [];
  assert.equal(await main(PUBLISH, { gh, env: { RUSTRIGHT_JOB_ID: "req-m1" }, admission: admitted, stdout: (l) => o.push(l), stderr: (l) => e.push(l), sleep: async () => {}, now: () => 0 }), 0);
  assert.match(e.join("\n"), /could not be written onto the claim/);
  assert.equal(claimOf(s2).state, "sending");
  const again = origin("req-m1", s2);
  assert.equal(await again.run(), 0); assert.equal(posts(s2), 1, "the retry found the run by its token and sent nothing");
});

test("[RA-05] listing errors and a run placed beyond the first page: the listing is paged, and an error or an exhausted page cap is pending, never 'no run'", async () => {
  const paged = newStore();
  const p = origin("req-m1", paged, { gh: { fillerRuns: 150 } });
  assert.equal(await p.run(), 0, p.err.join("\n"));
  assert.equal(JSON.parse(p.out[0]).runId, 5001, "the run on page 2 was found");
  assert.equal(paged.listings > 1, true);
  const beyondCap = newStore();
  const c = origin("req-m1", beyondCap, { gh: { fillerRuns: 1100 } });
  assert.equal(await c.run(), 2); assert.equal(pendingLine(c).code, "claim-sent-or-unknown"); assert.match(c.err.join("\n"), /unavailable or incomplete/);
  assert.equal(posts(beyondCap), 1); assert.equal(claimOf(beyondCap).state, "sending");
  const erroring = newStore();
  const e = origin("req-m1", erroring, { gh: { listFailsAfterPost: true } });
  assert.equal(await e.run(), 2); assert.equal(pendingLine(e).state, "sending");
  const again = origin("req-m1", erroring);
  assert.equal(await again.run(), 0); assert.equal(posts(erroring), 1);
});

test("[RA-05] a claim naming a run the Actions API does not confirm (or whose title lacks the effect token) is pending, not joined and not re-sent", async () => {
  const store = newStore();
  assert.equal(await origin("req-m1", store).run(), 0);
  store.runs.length = 0;
  const unconfirmed = origin("req-d1", store);
  assert.equal(await unconfirmed.run(), 2);
  assert.match(unconfirmed.err.join("\n"), /which the Actions API does not confirm/);
  assert.equal(posts(store), 1);
  store.runs.push({ id: 5001, run_attempt: 1, head_sha: SHA, event: "workflow_dispatch", path: ".github/workflows/publish.yml", display_title: "publish crates rightkit-daemon someone-else", visibleAt: 0 });
  const wrongToken = origin("req-d2", store);
  assert.equal(await wrongToken.run(), 2); assert.equal(posts(store), 1);
});

test("[R3-X04] the qualification tree is bound to the requested source before any claim or POST; an unreadable tree is not guessed", async () => {
  const wrongTree = qualFile({ tree: "e".repeat(40) });
  const bad = origin("req-m1", newStore(), { qualification: wrongTree });
  assert.equal(await bad.run(), 2);
  assert.match(bad.err.join("\n"), /refused \[tree-mismatch\]: the qualification receipt is for tree eeeeeeeeeeee, but commit aaaaaaaaaaaa has tree cccccccccccc; nothing was claimed or sent/);
  assert.ok(!bad.fake.calls.some((c) => c.includes("-X")), "no claim write and no POST");
  assert.equal(bad.fake.store.refs.size, 0);
  const unreadable = origin("req-m1", newStore(), { gh: { tree: null } });
  assert.equal(await unreadable.run(), 3);
  assert.match(unreadable.err.join("\n"), /cannot read the requested commit's tree/);
  assert.ok(!unreadable.fake.calls.some((c) => c.includes("-X")));
});

test("[R3-X03] every exported mode: probe sends nothing; ci is a typed refusal in production and takes the same claim path when admission is injected; publish is claimed", async () => {
  // ci under production admission: typed refusal (covered above); under injected admission: claimed by commit, one POST, a repeat joins, the tree is bound.
  const store = newStore();
  const CI = ["--workflow", "ci", "--sha", SHA];
  const first = origin("req-c1", store, { argv: CI });
  assert.equal(await first.run(), 0, first.err.join("\n"));
  const second = origin("req-c2", store, { argv: CI });
  assert.equal(await second.run(), 0);
  assert.equal(posts(store), 1); assert.equal(store.posts[0].workflow, "ci.yml");
  assert.equal(JSON.parse(second.out[0]).joined, true);
  const [ref] = [...store.refs.keys()];
  assert.match(ref, new RegExp(`^refs/rightkit-claims/ci/[0-9a-f]{16}/${SHA}$`));
  assert.equal(claimOf(store, ref).identity.tree, TREE);
  assert.equal(claimOf(store, ref).identity.workflow, "ci.yml");
  // A publish of the same commit is a different conflict key and is not blocked by the ci claim.
  const pub = origin("req-p1", store);
  assert.equal(await pub.run(), 0); assert.equal(posts(store), 2); assert.equal(store.refs.size, 2);
  // npm scope refusal precedes every effect.
  const npm = origin("req-n1", newStore(), { argv: ["--workflow", "publish", "--registry", "npm", "--unit", "leftpad", "--sha", SHA, "--qualification", QFILE], deps: { admission: undefined } });
  assert.equal(await npm.run(), 2); assert.match(npm.err.join("\n"), /preflight refused/); assert.ok(!npm.fake.calls.some((c) => c.includes("-X")));
  // A probe never touches the claim namespace or the dispatch route.
  const probe = fakeGh();
  assert.equal(await main(["--probe"], { gh: probe.gh, probeGh: () => ({ ok: true, stdout: "" }), env, stdout: () => {}, stderr: () => {} }), 0);
  assert.equal(probe.calls.length, 0);
});

test("[GB-47] readClaim distinguishes absent (an explicit empty listing) from unavailable (an error)", () => {
  const key = claimKey({ registry: "crates", unit: "rightkit-daemon", version: "0.4.0" });
  assert.equal(readClaim({ gh: fakeGh().gh, env: {}, repo: REPO, key }).status, "absent");
  assert.equal(readClaim({ gh: fakeGh({ claimsOk: false }).gh, env: {}, repo: REPO, key }).status, "unavailable");
});

// ---- rustright#200 (Adrian): a sent-or-unknown claim reconciles automatically by its request token and stays UNKNOWN; no empty/partial/failed lookup is "not sent" ----

const lostAck = (store, time, gh = {}) => origin("req-m1", store, { time, gh: { dispatchMode: "drop-response", visibleAfterListings: 100_000, ...gh } });

test("[#200] an empty listing, an expired visibility window and a listing error are UNKNOWN, recorded explicitly on the claim; nothing is ever re-sent, however much time passes", async () => {
  const store = newStore();
  const time = newClock();
  // Confirmed POST whose run stays invisible for the whole discovery window: the window expiring is not proof of non-dispatch.
  const sent = origin("req-m1", store, { time, gh: { visibleAfterListings: 100_000 } });
  assert.equal(await sent.run(), 2);
  assert.match(sent.err.join("\n"), /pending, dispatched but no publish\.yml run/);
  assert.equal(pendingLine(sent).reconciliation, "UNKNOWN");
  assert.deepEqual([claimOf(store).state, claimOf(store).reconciliation.state, claimOf(store).reconciliation.reason, claimOf(store).reconciliation.attempts], ["sending", "unknown", "no-run-visible", 1]);
  // Later requests (another origin, the same request id) inside the record throttle keep the state and send nothing.
  for (const id of ["req-d1", "req-m1"]) { const o = origin(id, store, { time }); assert.equal(await o.run(), 2); assert.equal(pendingLine(o).reconciliation, "UNKNOWN"); }
  assert.equal(claimOf(store).reconciliation.attempts, 1, "the unknown record is throttled, not rewritten on every request");
  // A listing ERROR on a later request: the lookup is retried (RECONCILE_LOOKUPS = 3) and the claim stays UNKNOWN with the error reason.
  time.c.t += 61_000;
  store.listingDown = true;
  const before = store.listings;
  const errored = origin("req-d2", store, { time });
  assert.equal(await errored.run(), 2);
  assert.equal(store.listings - before, 3, "an errored lookup is retried, not accepted once");
  assert.equal(pendingLine(errored).reconciliation, "UNKNOWN");
  assert.equal(claimOf(store).reconciliation.reason, "listing-unavailable-or-incomplete");
  assert.equal(claimOf(store).reconciliation.attempts, 2);
  store.listingDown = false;
  // Thirty days later the claim is still UNKNOWN and still not re-sendable: only the run (found by its token) can resolve it.
  time.c.t += 30 * 24 * 3_600_000;
  const much = origin("req-d3", store, { time });
  assert.equal(await much.run(), 2);
  assert.equal(posts(store), 1, "no timeout, empty listing or error ever licenses a second POST");
  assert.equal(claimOf(store).state, "sending");
  // The run becomes visible: the next request reconciles AUTOMATICALLY, joins it, and records it (found marks it sent).
  store.runs[0].visibleAt = 0;
  const later = origin("req-d4", store, { time });
  assert.equal(await later.run(), 0, later.err.join("\n"));
  assert.equal(JSON.parse(later.out[0]).joined, true); assert.equal(JSON.parse(later.out[0]).requestId, "req-m1");
  assert.equal(posts(store), 1);
  assert.deepEqual([claimOf(store).state, claimOf(store).reconciliation.state], ["dispatched", "found"]);
});

test("[#200] a lost acknowledgement is reconciled by a PAGED lookup on a later request: a run on page 2 is found and joined without a POST", async () => {
  const store = newStore();
  const time = newClock();
  const first = lostAck(store, time, { fillerRuns: 150 });
  assert.equal(await first.run(), 3);
  assert.equal(claimOf(store).reconciliation.state, "unknown");
  store.runs[0].visibleAt = 0;
  const calls = store.listings;
  const retry = origin("req-m1", store, { time, gh: { fillerRuns: 150 } });
  assert.equal(await retry.run(), 0, retry.err.join("\n"));
  assert.ok(store.listings - calls >= 2, "both pages were read");
  assert.equal(posts(store), 1);
  assert.equal(JSON.parse(retry.out[0]).runId, 5001);
  assert.equal(claimOf(store).reconciliation.state, "found");
});

test("[#200] a partially readable listing (page cap reached) is never 'not sent' and never a join of one seen run; the claim stays UNKNOWN", async () => {
  const store = newStore();
  const time = newClock();
  const first = lostAck(store, time, { fillerRuns: 1100 });
  assert.equal(await first.run(), 3);
  store.runs[0].visibleAt = 0;
  const retry = origin("req-d1", store, { time, gh: { fillerRuns: 1100 } });
  assert.equal(await retry.run(), 2);
  assert.equal(pendingLine(retry).reconciliation, "UNKNOWN");
  assert.equal(posts(store), 1);
  assert.equal(claimOf(store).state, "sending");
});

test("[#200] there is no manual release path: no flag, and a sending claim is never reclaimed by a stale window, a takeover or a force", async () => {
  for (const flag of ["--release", "--force", "--resend", "--reset"]) assert.throws(() => parseArgs(["--workflow", "ci", "--sha", SHA, flag]), /unknown argument/);
  const store = newStore();
  const time = newClock();
  assert.equal(await lostAck(store, time).run(), 3);
  time.c.t += 10 * CLAIM_STALE_MS;
  const same = origin("req-m1", store, { time });
  const other = origin("req-d1", store, { time });
  assert.deepEqual([await same.run(), await other.run()], [2, 2]);
  assert.equal(posts(store), 1, "stale takeover is only for a reserved claim that never sent");
  assert.equal(claimOf(store).takeover, undefined);
});
