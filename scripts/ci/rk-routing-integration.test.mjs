import assert from "node:assert/strict";
import test from "node:test";
import { crateRows, receiptRefusal, recheckHostedAdmission, nativeForecastClient, SCHEMA } from "./rk-preflight.mjs";
import { dispatchRefusal } from "./rk-pipeline.mjs";
import { main as dispatch } from "./rightkit-dispatch.mjs";

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
  const code = await dispatch(["--workflow", "publish", "--registry", "crates", "--unit", unit, "--sha", sha], {
    env: { RIGHTKIT_JOB_ID: "req-routing" }, stdout: () => {}, stderr: () => {}, now: () => 0,
    admission: async () => recheckHostedAdmission(input(hosted()), {
      checks: async (...args) => { events.push("checks"); return checks(...args); },
      observeRoute: async actual => { assert.equal(actual.sha, sha); assert.deepEqual(actual.rows, rows); events.push("route"); return freshCi(); },
    }),
    gh: args => {
      if (args.some(arg => arg.includes("/commits/main"))) return { ok: true, stdout: sha };
      if (args.includes("POST")) { events.push("POST"); return { ok: true, stdout: "" }; }
      return { ok: true, stdout: JSON.stringify({ workflow_runs: [{ id: 42, head_sha: sha, event: "workflow_dispatch",
        path: ".github/workflows/publish.yml", display_title: "publish req-routing", run_attempt: 1, html_url: "https://example.invalid/run/42" }] }) };
    },
  });
  assert.equal(code, 0);
  assert.deepEqual(events, ["checks", "route", "POST"]);
});
