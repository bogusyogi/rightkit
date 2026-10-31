import assert from "node:assert/strict";
import test from "node:test";
import { crateRows, receiptRefusal, recheckHostedAdmission, nativeForecastClient, SCHEMA } from "./rk-preflight.mjs";
import { dispatchRefusal } from "./rk-pipeline.mjs";
import { main as dispatch } from "./rightkit-dispatch.mjs";

const gitTree = "e".repeat(40); // a real git tree id is 40 hex: the dispatcher binds the receipt tree to the commit tree read from GitHub
const sha = "a".repeat(40), unit = "rightkit-example", version = "1.0.0", config = {};
const rows = crateRows(unit, config);
const expected = { sha, unit, version, config, registry: "crates" };
const input = receipt => ({ receipt, root: process.cwd(), sha, unit, version, config, rows });
const hosted = () => ({ ...expected, schema: SCHEMA, mode: "hosted-compile", route: "ci", ok: true,
  finishedAt: "2026-10-23T00:00:00.000Z", scans: { ok: true, exit: 0, timedOut: false },
  planChecks: { ok: true, exit: 0, timedOut: false }, requiredRows: rows.map(row => row.id), rows: [],
  routing: { route: "ci", local: { totalMs: 999999 }, hosted: { estimateMs: 1 } } });
const freshCi = () => ({ route: "ci", local: { knownHistory: true, totalMs: 2000 }, hosted: { estimateMs: 1000 } });
const checks = async (argv, deps) => {
  assert.deepEqual(argv, ["--registry", "crates", "--unit", unit]);
  assert.equal(deps.checksOnly, true);
  assert.equal(deps.writeReceipt, false);
  deps.onChecksPassed({ sha, unit });
  return 0;
};

test("native client selection uses enrolled path independently of adapter HOME", () => {
  const clientPath = "/Users/actual/Library/Application Support/RustRight/runtime/rustright";
  assert.equal(nativeForecastClient({ attestation: { darwin: { clientPath } } }, "darwin"), clientPath);
  assert.throws(() => nativeForecastClient({}, "darwin"), /not enrolled/);
  assert.throws(() => nativeForecastClient({ attestation: { win32: { clientPath: "D:\\agent-bin\\rustright.cmd" } } }, "win32"), /not enrolled/);
});

test("hosted receipt is explicit structural scheduling context, never local compile proof", () => {
  assert.ok(receiptRefusal(hosted(), expected));
  assert.equal(receiptRefusal(hosted(), { ...expected, allowHostedCompile: true }), null);
  assert.equal(dispatchRefusal(hosted(), sha, unit, "crates", config, version), null);
  for (const mutate of [r => { r.sha = "b".repeat(40); }, r => { r.scans.ok = false; },
    r => { r.planChecks.exit = 1; }, r => { r.registry = "npm"; }, r => { r.version = "9.9.9"; },
    r => { r.requiredRows.reverse(); }, r => { r.rows.push({ id: "forged" }); }]) {
    const receipt = hosted(); mutate(receipt);
    assert.ok(receiptRefusal(receipt, { ...expected, allowHostedCompile: true }));
  }
});

test("forged CI observations cannot waive checks or fresh local/unknown route", async () => {
  for (const route of [{ route: "local" }, { route: "ci", local: { knownHistory: false } },
    { route: "ci", local: { knownHistory: true, totalMs: 10 }, hosted: { estimateMs: 20 } }]) {
    await assert.rejects(recheckHostedAdmission(input(hosted()), { checks, observeRoute: async () => route }), /local compilation/);
  }
  let queried = false;
  await assert.rejects(recheckHostedAdmission(input(hosted()), {
    checks: async () => 1, observeRoute: async () => { queried = true; return freshCi(); },
  }), /deterministic/);
  assert.equal(queried, false);
  await assert.rejects(recheckHostedAdmission(input(hosted()), {
    checks: async (_argv, deps) => { deps.onChecksPassed({ sha: "b".repeat(40), unit }); return 0; },
    observeRoute: async () => freshCi(),
  }), /identity changed/);
  const red = hosted(); red.scans.exit = 1;
  await assert.rejects(recheckHostedAdmission(input(red), { checks, observeRoute: async () => freshCi() }), /scans/);
});

test("broker dispatch rechecks deterministic admission then real-observation route before sole hosted POST", async () => {
  const events = [];
  const claims = { refs: new Map(), commits: new Map() };
  let posted = false;
  const code = await dispatch(["--workflow", "publish", "--registry", "crates", "--unit", unit, "--sha", sha, "--qualification", "/bundle/qualification.json"], {
    env: { RUSTRIGHT_JOB_ID: "req-routing" }, stdout: () => {}, stderr: () => {}, now: () => 0,
    readQualification: (file, { sha: admitted, unit: name }) => { assert.equal(file, "/bundle/qualification.json"); assert.equal(admitted, sha); events.push("qualification");
      return { schema: "rightkit.unit-qualification/1", commit: sha, tree: gitTree, runtime: null, executor: "broker", unit: { kind: "cargo", name, version: "0.1.0" }, stages: { check: { ok: true }, compile: { ok: true }, package: { ok: true }, qualify: { ok: true } }, artifactSha256: "d".repeat(64) }; },
    admission: async () => recheckHostedAdmission(input(hosted()), {
      checks: async (...args) => { events.push("checks"); return checks(...args); },
      observeRoute: async actual => { assert.equal(actual.sha, sha); assert.deepEqual(actual.rows, rows); events.push("route"); return freshCi(); },
    }),
    gh: args => {
      const url = args.find(arg => typeof arg === "string" && arg.startsWith("repos/")) ?? "";
      const verb = args.includes("-X") ? args[args.indexOf("-X") + 1] : "GET";
      const field = key => { const i = args.findIndex((a, j) => (a === "-f" || a === "-F") && String(args[j + 1]).startsWith(`${key}=`)); return i < 0 ? undefined : args[i + 1].slice(key.length + 1); };
      const ok = body => ({ ok: true, stdout: JSON.stringify(body) });
      if (url.endsWith("/commits/main")) return { ok: true, stdout: sha };
      // R3-X04: the dispatcher binds the qualification tree to the requested commit's real tree (read from GitHub) before any claim or POST.
      if (url.endsWith(`/git/commits/${sha}`)) return ok({ sha, tree: { sha: gitTree } });
      // The shared candidate claim (GB-47) is GitHub-side bookkeeping, not a hosted run: refs, claim commits and the force=false swap are answered in
      // memory with GitHub's semantics, and none of them is "the POST". Only the workflow /dispatches request is.
      if (/\/git\/matching-refs\//.test(url)) return ok([...claims.refs].filter(([ref]) => ref.startsWith(`refs/${url.split("/git/matching-refs/")[1].split("?")[0]}`)).map(([ref, object]) => ({ ref, object: { sha: object, type: "commit" } })));
      if (/\/git\/commits$/.test(url) && verb === "POST") { const made = `${(claims.commits.size + 1).toString(16)}`.padStart(40, "0"); claims.commits.set(made, { message: field("message"), tree: field("tree"), parents: args.filter((a, j) => args[j - 1] === "-f" && String(a).startsWith("parents[]=")).map(a => a.slice(10)) }); return ok({ sha: made }); }
      const commit = /\/git\/commits\/([0-9a-f]{40})$/.exec(url);
      if (commit && verb === "GET") { const c = claims.commits.get(commit[1]); return c ? ok({ sha: commit[1], message: c.message, tree: { sha: c.tree }, parents: c.parents.map(p => ({ sha: p })) }) : { ok: false, stdout: "" }; }
      if (/\/git\/refs$/.test(url) && verb === "POST") { const ref = field("ref"); if (claims.refs.has(ref)) return { ok: false, stdout: "" }; claims.refs.set(ref, field("sha")); return ok({ ref }); }
      const patch = /\/git\/refs\/(.+)$/.exec(url);
      if (patch && verb === "PATCH") {
        assert.ok(args.includes("force=false"), "the claim swap is a conditional (fast-forward only) update");
        const ref = `refs/${patch[1]}`; const current = claims.refs.get(ref); const next = field("sha");
        if (!current || claims.commits.get(next)?.parents[0] !== current) return { ok: false, stdout: "" };
        claims.refs.set(ref, next); return ok({ ref });
      }
      if (/\/actions\/workflows\/publish\.yml\/dispatches$/.test(url) && verb === "POST") { events.push("POST"); posted = true; return { ok: true, stdout: "" }; }
      if (/\/actions\/runs\?/.test(url)) return ok({ total_count: posted ? 1 : 0, workflow_runs: posted ? [{ id: 42, head_sha: sha, event: "workflow_dispatch",
        path: ".github/workflows/publish.yml", display_title: "publish req-routing", run_attempt: 1, html_url: "https://example.invalid/run/42" }] : [] });
      throw new Error(`unexpected gh ${args.join(" ")}`);
    },
  });
  assert.equal(code, 0);
  assert.deepEqual(events, ["checks", "route", "qualification", "POST"], "the qualification receipt is re-read after admission and before the one POST");
  assert.equal(claims.refs.size, 1, "one claim for the candidate");
  assert.equal(JSON.parse(claims.commits.get([...claims.refs.values()][0]).message).state, "dispatched", "the run was recorded on the claim by a conditional swap");
});
